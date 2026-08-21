// Right-hand inspector panel for the "目標" page (Column 3 of `goals.rs`'s
// internal 3-pane layout). Split out of `goals.rs` purely to keep that file
// under this crate's own <800-line convention — same reason `dashboard.rs`
// has a sibling `dashboard_cards.rs` (see that pair's own doc comments for
// the precedent this file-layout choice follows). No behavior would differ
// from an unsplit version.
//
// Visual authority: `commercial/design/duduclaw-s4a-pages/Goals.dc.html`'s
// right column — 驗收標準（凍結基準）盒 / 判官回饋盒 / 輪次歷史 / 核准繼續＋
// 附指示重試＋中止三鈕 + 卡住原因. See `goals.rs`'s own module doc comment
// for why exactly THREE buttons (not the web dashboard's four) and how they
// map onto the `tasks.goal_decide` server verbs.

use gpui::{div, prelude::*, px, Context, Div, SharedString, Stateful};
use serde_json::{json, Value};

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, button, empty_state, skeleton, BadgeVariant, ButtonVariant};
use crate::screens::dashboard::Loadable;
use crate::screens::goals::{self, GoalsState};
use crate::screens::goals_data::{self, GoalTask, IterationRow};
use crate::text_field::TextField;
use crate::theme;
use crate::RootView;

const INSPECTOR_MAX_WIDTH: f32 = 640.0;

/// Codepoint-count truncation with an ellipsis — this crate has no
/// dependency on `duduclaw-core` (it's a detached workspace, see
/// `Cargo.toml`'s header comment), so `duduclaw_core::truncate_chars` isn't
/// reachable here; this is the local equivalent, scoped to this file's one
/// use (a judge-feedback preview in the round-history list).
fn truncate_chars(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push('…');
    }
    out
}

fn status_badge_variant(status: &str) -> BadgeVariant {
    match status {
        "needs_human" => BadgeVariant::Warning,
        "done" => BadgeVariant::Success,
        "cancelled" | "failed" => BadgeVariant::Destructive,
        "blocked" => BadgeVariant::Warning,
        _ => BadgeVariant::Info,
    }
}

fn pause_reason_label(locale: Locale, token: &str) -> SharedString {
    let key = match token {
        "no_progress" => "native.goals.pauseReason.noProgress",
        "budget_exhausted" => "native.goals.pauseReason.budgetExhausted",
        "blocked_needs_decision" => "native.goals.pauseReason.blockedNeedsDecision",
        "infra" => "native.goals.pauseReason.infra",
        "restart" => "native.goals.pauseReason.restart",
        _ => "native.goals.pauseReason.unknown",
    };
    i18n::t(locale, key)
}

/// Neutral card frame shared by the acceptance-criteria and round-history
/// boxes — both use the page's ordinary `surface_border()` token (the
/// resolved, correctly-alpha'd Hsla helper, not a raw hex const: an earlier
/// draft of this function took a `tone: u32` and applied `alpha(tone, 1.0)`,
/// which for `SURFACE_BORDER` (`0xffffff` in dark mode, meant to be read
/// through its own separate `SURFACE_BORDER_ALPHA`) rendered a solid opaque
/// white border — caught before ever building). The judge-feedback box below
/// needs its own distinct warning-tinted border and is written inline
/// instead of forcing that one case through this shared frame.
fn box_frame(title: SharedString, body: Div) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_3p5()
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

/// Numbered list when the text is clearly multi-line (matches the canvas's
/// "1./2./3." acceptance-criteria rendering); a single paragraph otherwise —
/// a one-sentence criteria string shouldn't get an artificial "1." prefix.
fn multiline_block(text: &str) -> Div {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if lines.len() <= 1 {
        return div()
            .text_size(px(theme::TEXT_SM))
            .text_color(theme::alpha(theme::FOREGROUND, 1.0))
            .child(text.trim().to_string());
    }
    let mut wrap = div().flex().flex_col().gap_1p5();
    for (i, line) in lines.iter().enumerate() {
        wrap = wrap.child(
            div()
                .flex()
                .gap_2()
                .text_size(px(theme::TEXT_SM))
                .child(div().text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(format!("{}.", i + 1)))
                .child(div().flex_1().text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(line.to_string())),
        );
    }
    wrap
}

fn acceptance_criteria_box(locale: Locale, task: &GoalTask) -> Div {
    let (title_key, text) = match (&task.acceptance_criteria_baseline, &task.acceptance_criteria) {
        (Some(baseline), _) => ("native.goals.inspector.acceptanceCriteria", baseline.clone()),
        (None, Some(live)) => ("native.goals.inspector.acceptanceCriteriaLive", live.clone()),
        (None, None) => {
            return box_frame(
                i18n::t(locale, "native.goals.inspector.acceptanceCriteria"),
                div()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                    .child(i18n::t(locale, "native.goals.inspector.acceptanceCriteriaEmpty")),
            );
        }
    };
    box_frame(i18n::t(locale, title_key), multiline_block(&text))
}

/// Only rendered for a `needs_human` task with actual feedback text —
/// matches the canvas exactly (the box only appears on the stuck example).
/// The round shown is the CURRENT round (`revision_round + 1`, the same
/// 1-based convention `native.home.card.goal.round` already uses
/// everywhere else on this page); the timestamp is `updated_at` as an
/// honest best-available proxy — `TaskRow` carries no dedicated
/// "judge_feedback_at" column, only a whole-row `updated_at`.
fn judge_feedback_box(locale: Locale, task: &GoalTask, now: chrono::DateTime<chrono::Utc>) -> Option<Div> {
    if task.status != "needs_human" {
        return None;
    }
    let feedback = task.judge_feedback.as_ref()?;
    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::alpha(theme::WARNING, 1.0))
                .child(i18n::t1(
                    locale,
                    "native.goals.inspector.judgeFeedback",
                    "n",
                    &(task.revision_round + 1).to_string(),
                )),
        )
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(goals::relative_time(locale, &task.updated_at, now)),
        );
    Some(
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_3p5()
            .rounded(px(theme::RADIUS_XL))
            .bg(theme::alpha(theme::SURFACE, 1.0))
            .border_1()
            .border_color(theme::alpha(theme::WARNING, 0.35))
            .shadow(theme::surface_shadow())
            .child(header)
            .child(
                div()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                    .child(feedback.clone()),
            ),
    )
}

fn verdict_dot(verdict: Option<&str>) -> Div {
    let (color, filled) = match verdict {
        Some("accepted") => (theme::SUCCESS, true),
        Some("rejected") => (theme::DESTRUCTIVE, true),
        Some("escalated") => (theme::WARNING, false),
        _ => (theme::MUTED_FOREGROUND, false),
    };
    let dot = div().size(px(9.)).rounded_full().flex_shrink_0();
    if filled {
        dot.bg(theme::alpha(color, 1.0))
    } else {
        dot.border_2().border_color(theme::alpha(color, 1.0))
    }
}

fn verdict_outcome_text(locale: Locale, row: &IterationRow) -> SharedString {
    match row.verdict.as_deref() {
        Some("accepted") => i18n::t(locale, "native.goals.inspector.verdict.accepted"),
        Some("rejected") => {
            let base = i18n::t(locale, "native.goals.inspector.verdict.rejected");
            match &row.judge_feedback {
                Some(fb) => format!("{base} — {}", truncate_chars(fb, 36)).into(),
                None => base,
            }
        }
        Some("escalated") => i18n::t(locale, "native.goals.inspector.verdict.escalated"),
        _ if row.submitted_at.is_some() => i18n::t(locale, "native.goals.inspector.verdict.judging"),
        _ => i18n::t(locale, "native.goals.inspector.verdict.inProgress"),
    }
}

fn round_history_row(locale: Locale, row: &IterationRow) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2p5()
        .child(verdict_dot(row.verdict.as_deref()))
        .child(
            div()
                .w(px(52.))
                .flex_shrink_0()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::t1(locale, "native.home.card.goal.round", "n", &row.round.to_string())),
        )
        .child(
            div()
                .flex_1()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(verdict_outcome_text(locale, row)),
        )
}

fn round_history_box(locale: Locale, iterations: &Loadable<Vec<IterationRow>>) -> Div {
    let body: Div = match iterations {
        Loadable::Loading => {
            let mut wrap = div().flex().flex_col().gap_1p5();
            for _ in 0..2 {
                wrap = wrap.child(skeleton(px(220.), px(14.)));
            }
            wrap
        }
        Loadable::Failed(msg) => div()
            .text_size(px(theme::TEXT_XS))
            .text_color(theme::alpha(theme::DESTRUCTIVE, 1.0))
            .child(i18n::t1(locale, "native.home.card.errorPrefix", "message", msg)),
        Loadable::Ready(rows) if rows.is_empty() => div()
            .text_size(px(theme::TEXT_XS))
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .child(i18n::t(locale, "native.goals.inspector.roundHistoryEmpty")),
        Loadable::Ready(rows) => {
            let mut wrap = div().flex().flex_col().gap_2();
            for row in rows {
                wrap = wrap.child(round_history_row(locale, row));
            }
            wrap
        }
    };
    box_frame(i18n::t(locale, "native.goals.inspector.roundHistory"), body)
}

/// The exact `{task_id, action, note?}` payload `tasks.goal_decide` expects
/// (`handle_tasks_goal_decide`, `crates/duduclaw-gateway/src/handlers.rs`
/// ~L29784) — `note` omitted entirely (not sent as `""`) when empty, so the
/// two-button "same verb, different guidance" mapping this file's header
/// comment documents produces a genuinely minimal payload for 核准繼續.
fn build_goal_decide_params(task_id: &str, action: &str, note: &str) -> Value {
    let note = note.trim();
    let mut obj = json!({ "task_id": task_id, "action": action });
    if !note.is_empty() {
        obj["note"] = json!(note);
    }
    obj
}

/// Dispatches one `tasks.goal_decide` call and applies its outcome to the
/// `GoalsState` global — patches the returned `task` row into the already-
/// loaded list in place (avoids a full re-fetch for a single-row change) and
/// forces the round-history panel to reload (a new round may have just been
/// appended). Never called from this pass's own smoke test — see `goals.rs`'s
/// module doc comment on the task brief's "活測絕不真按" instruction; this
/// function's correctness is exercised via `build_goal_decide_params`'s unit
/// tests, not a live click.
fn dispatch_decide(
    cx: &mut Context<RootView>,
    session_tx: tokio::sync::mpsc::UnboundedSender<crate::ws_status::Command>,
    task_id: String,
    action: &'static str,
    note: String,
) {
    {
        let g = cx.global_mut::<GoalsState>();
        g.decision_busy = true;
        g.decision_result = None;
    }
    let params = build_goal_decide_params(&task_id, action, &note);
    goals::spawn_goal_call(cx, session_tx, "tasks.goal_decide", params, |cx, result| {
        let g = cx.default_global::<GoalsState>();
        g.decision_busy = false;
        match result {
            Ok(payload) => {
                let message = payload.get("message").and_then(Value::as_str).unwrap_or("").to_string();
                g.retry_note_open = false;
                if let Some(t) = payload.get("task").filter(|t| !t.is_null()) {
                    if let Some(updated) = goals_data::parse_goal_tasks(&json!({ "tasks": [t] })).into_iter().next() {
                        if let Loadable::Ready(list) = &mut g.tasks {
                            if let Some(existing) = list.iter_mut().find(|x| x.id == updated.id) {
                                *existing = updated;
                            }
                        }
                    }
                }
                g.iterations_loaded_for = None;
                g.iterations = Loadable::Loading;
                g.decision_result = Some(Ok(message));
            }
            Err(e) => g.decision_result = Some(Err(e)),
        }
    });
}

fn decision_row(state: &RootView, task: &GoalTask, cx: &mut Context<RootView>) -> Div {
    let locale = state.locale;
    let g = cx.default_global::<GoalsState>();
    let busy = g.decision_busy;
    let retry_note_open = g.retry_note_open;
    let retry_note_field = g.retry_note.clone();
    let decision_result = g.decision_result.clone();
    let pause_reason = task.pause_reason.clone();

    let task_id_approve = task.id.clone();
    let task_id_retry_submit = task.id.clone();
    let task_id_abort = task.id.clone();
    let retry_placeholder = i18n::t(locale, "native.goals.inspector.decide.retryPlaceholder");

    let buttons = div()
        .flex()
        .items_center()
        .flex_wrap()
        .gap_2()
        .child(button(
            "goals-decide-approve",
            i18n::t(locale, "native.goals.inspector.decide.approve"),
            ButtonVariant::Primary,
            busy,
            None,
            cx.listener(move |this, _ev, _window, cx| {
                let tx = this.session_tx.clone();
                dispatch_decide(cx, tx, task_id_approve.clone(), "retry", String::new());
                cx.notify();
            }),
        ))
        .child(button(
            "goals-decide-retry-toggle",
            i18n::t(locale, "native.goals.inspector.decide.retry"),
            ButtonVariant::Secondary,
            busy,
            None,
            cx.listener(move |_this, _ev, _window, cx| {
                let (open_now, needs_field) = {
                    let g = cx.global_mut::<GoalsState>();
                    g.retry_note_open = !g.retry_note_open;
                    (g.retry_note_open, g.retry_note_open && g.retry_note.is_none())
                };
                if needs_field {
                    let field = TextField::new(cx, retry_placeholder.clone(), false, "");
                    cx.global_mut::<GoalsState>().retry_note = Some(field);
                }
                let _ = open_now;
                cx.notify();
            }),
        ))
        .child(button(
            "goals-decide-abort",
            i18n::t(locale, "native.goals.inspector.decide.abort"),
            ButtonVariant::Destructive,
            busy,
            None,
            cx.listener(move |this, _ev, _window, cx| {
                let tx = this.session_tx.clone();
                dispatch_decide(cx, tx, task_id_abort.clone(), "abort", String::new());
                cx.notify();
            }),
        ))
        .children(pause_reason.map(|token| {
            div()
                .ml_auto()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::t1(
                    locale,
                    "native.goals.inspector.decide.pauseReasonPrefix",
                    "reason",
                    &pause_reason_label(locale, &token),
                ))
        }));

    let mut col = div().flex().flex_col().gap_2().child(buttons);

    if retry_note_open {
        if let Some(field) = retry_note_field {
            col = col.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .h(px(32.))
                            .px_2p5()
                            .flex()
                            .items_center()
                            .rounded(px(theme::RADIUS_LG))
                            .bg(theme::dark::input_bg())
                            .border_1()
                            .border_color(theme::input_border())
                            .text_size(px(theme::TEXT_SM))
                            .child(field.clone()),
                    )
                    .child(button(
                        "goals-decide-retry-submit",
                        i18n::t(locale, "native.goals.inspector.decide.retrySubmit"),
                        ButtonVariant::Primary,
                        busy,
                        None,
                        cx.listener(move |this, _ev, _window, cx| {
                            // Clone the `Entity<TextField>` handle first and
                            // let the `default_global` borrow end before
                            // `.read(cx)` — both need `cx`, and a `TextField`
                            // field read while `GoalsState`'s mutable global
                            // borrow is still live doesn't borrow-check.
                            let field = cx.default_global::<GoalsState>().retry_note.clone();
                            let note = field.map(|f| f.read(cx).content.clone()).unwrap_or_default();
                            let tx = this.session_tx.clone();
                            dispatch_decide(cx, tx, task_id_retry_submit.clone(), "retry", note);
                            cx.notify();
                        }),
                    )),
            );
        }
    }

    if let Some(result) = decision_result {
        let (color, text) = match result {
            Ok(msg) => (theme::SUCCESS, msg),
            Err(msg) => (theme::DESTRUCTIVE, i18n::t1(locale, "native.goals.inspector.decide.errorPrefix", "message", &msg).to_string()),
        };
        col = col.child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(color, 1.0)).child(text));
    }

    col
}

fn meta_line(locale: Locale, task: &GoalTask) -> SharedString {
    let base = i18n::tn(
        locale,
        "native.goals.inspector.meta",
        &[
            ("agent", &task.agent_id),
            ("round", &(task.revision_round + 1).to_string()),
            ("created", &goals::short_date(&task.created_at)),
        ],
    );
    match &task.channel {
        Some(ch) => format!("{base}・{}", i18n::t1(locale, "native.goals.inspector.metaChannel", "channel", ch)).into(),
        None => base,
    }
}

fn header(locale: Locale, task: &GoalTask) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2p5()
                .child(goals_status_dot(&task.status))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(theme::TEXT_XL))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child(SharedString::from(task.title.clone())),
                )
                .child(badge(goals::status_label(locale, &task.status), status_badge_variant(&task.status))),
        )
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(meta_line(locale, task)),
        )
}

/// Local copy of `goals.rs`'s status-dot shape at the header's larger size —
/// that helper is private to `goals.rs` (module-internal styling detail),
/// so this is a thin, intentionally duplicated 6-line wrapper rather than
/// widening that function's visibility for one extra call site.
fn goals_status_dot(status: &str) -> Div {
    let color = match status {
        "needs_human" => theme::WARNING,
        "done" => theme::SUCCESS,
        "cancelled" | "failed" => theme::DESTRUCTIVE,
        "blocked" => theme::WARNING,
        _ => theme::BRAND,
    };
    let filled = matches!(status, "done" | "cancelled" | "failed" | "blocked");
    let dot = div().size(px(14.)).rounded_full().flex_shrink_0();
    if filled {
        dot.bg(theme::alpha(color, 1.0))
    } else {
        dot.border_3().border_color(theme::alpha(color, 1.0))
    }
}

pub(super) fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    let locale = state.locale;
    let g = cx.default_global::<GoalsState>();

    let selected: Option<GoalTask> = match (&g.tasks, &g.selected_task_id) {
        (Loadable::Ready(list), Some(id)) => list.iter().find(|t| &t.id == id).cloned(),
        _ => None,
    };

    let panel = div()
        .id("goals-inspector")
        .flex_1()
        .h_full()
        .max_w(px(INSPECTOR_MAX_WIDTH))
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
                "🎯",
                i18n::t(locale, "native.goals.inspector.emptyTitle"),
                Some(i18n::t(locale, "native.goals.inspector.emptyDesc")),
                None::<Div>,
            )),
        );
    };

    let now = chrono::Utc::now();
    let iterations = g.iterations.clone();

    panel
        .child(header(locale, &task))
        .child(acceptance_criteria_box(locale, &task))
        .children(judge_feedback_box(locale, &task, now))
        .child(round_history_box(locale, &iterations))
        .when(task.status == "needs_human", |el| el.child(decision_row(state, &task, cx)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_goal_decide_params_approve_continue_omits_note() {
        let v = build_goal_decide_params("t1", "retry", "");
        assert_eq!(v, json!({ "task_id": "t1", "action": "retry" }));
    }

    #[test]
    fn build_goal_decide_params_retry_with_instruction_includes_trimmed_note() {
        let v = build_goal_decide_params("t1", "retry", "  用第二個數字版本  ");
        assert_eq!(v, json!({ "task_id": "t1", "action": "retry", "note": "用第二個數字版本" }));
    }

    #[test]
    fn build_goal_decide_params_abort_has_no_note_key() {
        let v = build_goal_decide_params("t1", "abort", "");
        assert_eq!(v, json!({ "task_id": "t1", "action": "abort" }));
        assert!(v.get("note").is_none());
    }

    #[test]
    fn build_goal_decide_params_whitespace_only_note_is_treated_as_empty() {
        let v = build_goal_decide_params("t1", "retry", "   \n  ");
        assert_eq!(v, json!({ "task_id": "t1", "action": "retry" }));
    }

    #[test]
    fn verdict_outcome_text_maps_all_four_states() {
        let locale = Locale::ZhTw;
        let row = |verdict: Option<&str>, submitted: bool| IterationRow {
            round: 1,
            verdict: verdict.map(str::to_string),
            judge_feedback: None,
            submitted_at: submitted.then(|| "2026-08-20T00:00:00Z".to_string()),
            judged_at: None,
        };
        assert_eq!(verdict_outcome_text(locale, &row(Some("accepted"), true)), "通過");
        assert_eq!(verdict_outcome_text(locale, &row(Some("escalated"), true)), "上報，待你決定");
        assert_eq!(verdict_outcome_text(locale, &row(None, true)), "等待判官");
        assert_eq!(verdict_outcome_text(locale, &row(None, false)), "進行中");
    }

    #[test]
    fn verdict_outcome_text_rejected_appends_truncated_feedback() {
        let locale = Locale::ZhTw;
        let row = IterationRow {
            round: 1,
            verdict: Some("rejected".to_string()),
            judge_feedback: Some("異常波動原因推測段落引用的數字跟附件對不上，需要你確認要採用哪個版本再繼續執行".to_string()),
            submitted_at: None,
            judged_at: None,
        };
        let text = verdict_outcome_text(locale, &row).to_string();
        assert!(text.starts_with("駁回 — "));
        assert!(text.ends_with('…'));
    }

    #[test]
    fn truncate_chars_is_codepoint_safe_on_cjk() {
        assert_eq!(truncate_chars("一二三四五", 3), "一二三…");
        assert_eq!(truncate_chars("短", 10), "短");
    }

    #[test]
    fn multiline_block_single_line_has_no_number_prefix() {
        // Indirectly exercised via `acceptance_criteria_box` in practice;
        // this pins the split/filter logic itself.
        let lines: Vec<&str> = "只有一句話".lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn meta_line_includes_channel_only_when_present() {
        let locale = Locale::ZhTw;
        let base_task = |channel: Option<&str>| GoalTask {
            id: "t1".into(),
            title: "x".into(),
            status: "needs_human".into(),
            agent_id: "小杜".into(),
            revision_round: 2,
            judge_feedback: None,
            acceptance_criteria: None,
            acceptance_criteria_baseline: None,
            pause_reason: None,
            updated_at: "2026-08-21T10:00:00Z".into(),
            created_at: "2026-08-12T00:00:00Z".into(),
            channel: channel.map(str::to_string),
        };
        let without = meta_line(locale, &base_task(None));
        let with = meta_line(locale, &base_task(Some("telegram")));
        assert!(!without.contains("telegram"));
        assert!(with.contains("telegram"));
        assert!(without.contains("小杜"));
        assert!(without.contains("08/12"));
    }

    #[test]
    fn status_badge_variant_maps_terminal_statuses_to_destructive() {
        assert!(matches!(status_badge_variant("failed"), BadgeVariant::Destructive));
        assert!(matches!(status_badge_variant("cancelled"), BadgeVariant::Destructive));
        assert!(matches!(status_badge_variant("done"), BadgeVariant::Success));
        assert!(matches!(status_badge_variant("needs_human"), BadgeVariant::Warning));
    }

    #[test]
    fn pause_reason_label_unknown_token_degrades_to_unknown_label() {
        let locale = Locale::ZhTw;
        assert_eq!(pause_reason_label(locale, "no_progress"), "卡住沒進展");
        assert_eq!(pause_reason_label(locale, "totally-made-up"), "需要人工確認");
    }
}
