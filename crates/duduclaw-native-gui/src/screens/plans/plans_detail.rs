// Right column: the selected plan's title + progress badge + description
// placeholder, a 步驟 card (status circle · text · assignee · reorder
// chevrons), and a "下一步要做什麼？" input row. Split out of `plans.rs` for
// the same file-size reason `routines.rs`/`routines_detail.rs` are split —
// see `plans.rs`'s module doc comment for the overall page design, RPC
// shapes, and the step-status-cycle / new-step-input scope notes.

use gpui::{div, prelude::*, px, Context, CursorStyle, Div, KeyDownEvent, MouseButton, SharedString, Stateful};

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, button, empty_state, skeleton, BadgeVariant, ButtonVariant};
use crate::screens::dashboard::Loadable;
use crate::theme;
use crate::RootView;

use super::{cycle_step_status, PlanDetail, PlanStep, PlansSnapshot, PlansState};

/// Status-circle color for one of [`super::PLAN_STEP_STATUSES`] — `done` =
/// filled success circle with a check glyph, `doing` = an outlined brand
/// ring, `todo`/`skipped`/anything unrecognized = a neutral outlined ring
/// (the canvas has no distinct `skipped` treatment, so it shares `todo`'s
/// visual rather than inventing an unsanctioned 5th look).
fn step_circle(status: &str) -> Div {
    match status {
        "done" => div()
            .size(px(17.))
            .flex_shrink_0()
            .rounded_full()
            .bg(theme::alpha(theme::SUCCESS, 1.0))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(10.))
            .text_color(theme::alpha(theme::PAGE_CANVAS, 1.0))
            .child("✓"),
        "doing" => div().size(px(17.)).flex_shrink_0().rounded_full().border_3().border_color(theme::alpha(theme::BRAND, 1.0)),
        _ => div().size(px(17.)).flex_shrink_0().rounded_full().border_2().border_color(theme::surface_border()),
    }
}

fn step_row(step: &PlanStep, locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    let assignee = if step.assignee.is_empty() { i18n::t(locale, "native.plans.step.unassigned") } else { step.assignee.clone().into() };
    let done = step.status == "done";
    let step_id = step.id.clone();
    let current_status = step.status.clone();

    div()
        .id(SharedString::from(format!("plans-step-{}", step.id)))
        .flex()
        .items_center()
        .gap_2p5()
        .py_1p5()
        // The status circle is the ONLY clickable surface on this row — the
        // whole row is not a giant hit target, matching the canvas's tight
        // circle-only affordance.
        .child(
            div()
                .id(SharedString::from(format!("plans-step-circle-{}", step.id)))
                .cursor_pointer()
                .child(step_circle(&step.status))
                .on_click(cx.listener(move |_this, _ev, _window, cx| {
                    let next = cycle_step_status(&current_status);
                    if let Loadable::Ready(detail) = &mut cx.global_mut::<PlansState>().detail {
                        if let Some(s) = detail.steps.iter_mut().find(|s| s.id == step_id) {
                            s.status = next.to_string();
                        }
                    }
                    cx.notify();
                })),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_size(px(theme::TEXT_SM))
                .when(done, |el| el.text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).line_through())
                .when(!done, |el| el.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
                .child(step.text.clone()),
        )
        .child(div().flex_shrink_0().w(px(72.)).text_right().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(assignee))
        // Reorder chevrons — decorative only (no drag/drop or `position`
        // RPC in this pass's scope, same "assembled, not wired" convention
        // as every button on this page).
        .child(
            div()
                .flex()
                .gap_0p5()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.6))
                .child("▲")
                .child("▼"),
        )
}

fn steps_card(detail: &PlanDetail, locale: Locale, cx: &mut Context<RootView>) -> Div {
    let mut list = div().flex().flex_col();
    for step in &detail.steps {
        list = list.child(step_row(step, locale, cx));
    }
    div()
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .rounded(px(theme::RADIUS_XL))
        .p_3()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(div().text_size(px(theme::TEXT_XS)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "native.plans.stepsHeader")))
        .child(list)
}

/// The "下一步要做什麼？" input — reuses `console_detail.rs::goal_note_field`'s
/// exact manual key-capture pattern (a cached `FocusHandle` + `on_key_down`
/// backspace/char handling), this crate's established substitute for real
/// IME composition outside the dedicated `ImeTextInput` entity. A real,
/// typeable field; the "新增步驟" button next to it stays inert — see
/// `plans.rs`'s module doc comment.
fn new_step_row(value: &str, locale: Locale, cx: &mut Context<RootView>) -> Div {
    let handle = ensure_focus_handle(cx);
    let handle_for_click = handle.clone();
    let is_empty = value.is_empty();

    let input_box = div()
        .id("plans-new-step-input")
        .track_focus(&handle)
        .key_context("PlansNewStep")
        .on_key_down(cx.listener(|_this, event: &KeyDownEvent, _window, cx| {
            let ks = &event.keystroke;
            if ks.modifiers.platform || ks.modifiers.control || ks.modifiers.function {
                return;
            }
            match ks.key.as_str() {
                "backspace" => {
                    cx.global_mut::<PlansState>().new_step_text.pop();
                    cx.notify();
                }
                _ => {
                    if let Some(ch) = ks.key_char.as_deref() {
                        if !ch.is_empty() && ch.chars().all(|c| !c.is_control()) {
                            cx.global_mut::<PlansState>().new_step_text.push_str(ch);
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
        .child(if is_empty { i18n::t(locale, "native.plans.addStepPlaceholder") } else { value.to_string().into() });

    div()
        .flex()
        .items_center()
        .gap_2()
        .child(input_box)
        // Assembled, not wired — see `plans.rs`'s module doc comment.
        .child(button("plans-add-step", i18n::t(locale, "native.plans.addStepButton"), ButtonVariant::Secondary, false, None, |_ev, _window, _app| {}))
}

/// Lazily-created `FocusHandle` for the new-step input, cached on
/// `PlansState` — same pattern `console.rs::ensure_focus_handle` establishes
/// (needs `&App`, unavailable at `PlansState::new()` construction time).
fn ensure_focus_handle(cx: &mut Context<RootView>) -> gpui::FocusHandle {
    if let Some(existing) = NEW_STEP_FOCUS.with(|c| c.borrow().clone()) {
        return existing;
    }
    let handle = cx.focus_handle();
    NEW_STEP_FOCUS.with(|c| *c.borrow_mut() = Some(handle.clone()));
    handle
}

thread_local! {
    /// Thread-local rather than a `PlansState` field: a `FocusHandle` is a
    /// live window resource, not view data, matching `console.rs`'s own
    /// `note_focus` (a `RefCell` field there since `ConsoleState` already
    /// exists as a struct with other fields to hang it off — this file has
    /// no such struct of its own, so a module-local `thread_local!` is the
    /// equivalent zero-`RootView`-change storage for a gpui app that is
    /// single-threaded per window).
    static NEW_STEP_FOCUS: std::cell::RefCell<Option<gpui::FocusHandle>> = const { std::cell::RefCell::new(None) };
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

fn description_line(detail: &PlanDetail, locale: Locale) -> Div {
    let text = if detail.plan.description.is_empty() {
        i18n::t(locale, "native.plans.descPlaceholder")
    } else {
        detail.plan.description.clone().into()
    };
    div().mt_1().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(text)
}

pub(super) fn detail_column(snap: &PlansSnapshot, locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    let body: Stateful<Div> = match &snap.detail {
        _ if snap.selected.is_none() => div().id("plans-detail-empty").flex_1().flex().items_center().justify_center().child(empty_state(
            "📋",
            i18n::t(locale, "native.plans.selectHint"),
            None,
            None::<Div>,
        )),
        Loadable::Loading => div()
            .id("plans-detail-loading")
            .flex_1()
            .flex()
            .flex_col()
            .gap_3()
            .p_5()
            .child(skeleton(px(220.), px(24.)))
            .child(skeleton(px(320.), px(120.))),
        Loadable::Failed(e) => div().id("plans-detail-error").flex_1().flex().flex_col().p_5().child(error_banner(locale, e)),
        Loadable::Ready(detail) => {
            let progress = i18n::tn(
                locale,
                "native.plans.progress",
                &[
                    ("done", &detail.steps.iter().filter(|s| s.status == "done" || s.status == "skipped").count().to_string()),
                    ("total", &detail.steps.len().to_string()),
                ],
            );
            div()
                .id("plans-detail-body")
                .flex_1()
                .flex()
                .flex_col()
                .gap_3()
                .p_5()
                .overflow_y_scroll()
                .child(
                    div()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(div().text_size(px(theme::TEXT_XL)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(detail.plan.title.clone()))
                                .child(badge(progress, BadgeVariant::Info)),
                        )
                        .child(description_line(detail, locale)),
                )
                .child(steps_card(detail, locale, cx))
                .child(new_step_row(&snap.new_step_text, locale, cx))
        }
    };

    div().id("plans-detail-column").flex_1().h_full().flex().flex_col().bg(theme::alpha(theme::PAGE_CANVAS, 0.4)).child(body)
}
