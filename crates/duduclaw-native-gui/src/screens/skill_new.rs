// WP-S6b2-M (S6b 第二波, 2026-08-21) — "新增技能" (`SkillNew.dc.html`, B16
// in-page wizard, sidebar retained). No `nav.rs` entry of its own — reached
// via `active_page == "skillNew"`; the canvas's own left sidebar shows
// "技能庫" highlighted as the owning area, so this page is a `skills.rs`
// drill-down conceptually, same "no own sidebar row, breadcrumb only"
// shape `mcp_keys.rs`/`identity.rs` already establish for a leaf reached
// from a sibling page's own row (`skills.rs`'s "＋新增技能" entry point is a
// parallel workstream's scope this task does not touch — reachable via
// `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=skillNew` today, same "D 先掛好分支就直接
// 可達，未掛就自己掛" precedent every prior self-attached S5b/S6b page
// already establishes). `shell.rs`'s own `active_id == "skillNew"` branch
// wraps this page in the normal `content_shell` (sidebar + content-list
// columns intact) — unlike `screens::migrate`'s full-bleed bypass, this is
// an ordinary content page.
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, never guessed) — cited for the record, none wired this
// pass (see below) ────────────────────────────────────────────────────────
//   `skills.custom_create {display_name,slug?,description_human?,
//     time_saved_value?,time_saved_unit?,tags?,built_by_agent?}` (dispatch
//     L6848, `handle_skills_custom_create` L16327, open to any logged-in
//     caller) → the new `CustomSkillRecord` (status `draft`).
//   `skills.custom_generate {id,agent?,instruction?}` (dispatch L6849,
//     `handle_skills_custom_generate` L16410) enqueues a `bus_queue.jsonl`
//     `agent_message` task asking the target agent to author a SKILL.md
//     into an isolated per-skill draft dir, flips the record to
//     `generating`, returns `{"success","id","message_id","target_agent",
//     "draft_path","status":"generating"}` — completion is NOT pushed back;
//     the real web wizard (`web/src/components/skills/SkillWizard.tsx`)
//     polls `skills.custom_list` every 3s waiting for the status to leave
//     `generating`.
//   `skills.custom_update {id,display_name?,description_human?,
//     time_saved_value?,time_saved_unit?,tags?}` (dispatch L6850,
//     `handle_skills_custom_update` L16503) edits the human-facing fields.
//   `skills.custom_submit {id}` (dispatch L6851, `handle_skills_custom_submit`
//     L16543) runs the mandatory safety scan and, on pass, routes to an
//     approver (`pending_approval`) — REJECTS outright on high/critical
//     risk (fail-closed).
//   `skills.custom_list {}` (dispatch L6852, `handle_skills_custom_list`
//     L16696) — admins see every record, others only their own.
//   `skills.custom_retire {id}` (dispatch L6853, `handle_skills_custom_retire`
//     L16743) — creator or admin only (checked inside the handler).
//
// ── Nothing wired this pass — an honest local-only wizard ────────────────
// Per this task's own brief ("生成/儲存決策類組裝不真按，步驟導航純 UI 態可
// 真切換"): every one of the six RPCs above is a WRITE (creates a record,
// enqueues an agent task, edits fields, or triggers a safety scan +
// approval routing) — this crate's own established "disabled `mds_gpui::
// button`, zero click handler" pattern for decision-type actions
// (`distributors.rs`/`users.rs`/`mail.rs`'s compose-send) applies to all
// six, not a subset. Only step navigation (the 4-dot stepper, 上一步/下一步
// buttons, and the canvas's own "確認草稿完成" link — which in this crate's
// port is exactly a local "unlock the next-step button" toggle, no RPC)
// is real state. Same "no RPC calls on this page at all" shape `manage_
// advanced.rs`'s own header comment already establishes for a page in this
// exact crate whose whole job is static navigation, not data. `describe`/
// `form`/`review` steps have no canvas reference (only `generate` — this
// task's own "展示重點" — is drawn) and render an honest minimal stub;
// `web/src/components/skills/SkillWizard.tsx`'s much richer 4-step form
// (agent picker, tag editor, 3s poll loop) is a functional cross-reference
// only, never a layout source, per this task's "版面禁抄 web" rule.
// `draftPath`/`agentLabel` values below are illustrative placeholders
// matching the canvas's own mock content ("skills/custom/reply-digest-
// summarizer/SKILL.md", "Coder"), not real per-request generation output.

use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};

use crate::i18n::{self, Locale};
use crate::mds_gpui::{button, ButtonVariant};
use crate::theme;
use crate::RootView;

const CONTENT_MAX_WIDTH: f32 = 640.0;

// ── State ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillNewStep {
    Describe,
    Generate,
    FormFields,
    Review,
}

const STEPS: [SkillNewStep; 4] = [SkillNewStep::Describe, SkillNewStep::Generate, SkillNewStep::FormFields, SkillNewStep::Review];

impl SkillNewStep {
    fn ordinal(self) -> u8 {
        STEPS.iter().position(|s| *s == self).unwrap_or(0) as u8
    }

    fn title_key(self) -> &'static str {
        match self {
            SkillNewStep::Describe => "skillNew.step.describe",
            SkillNewStep::Generate => "skillNew.step.generate",
            SkillNewStep::FormFields => "skillNew.step.form",
            SkillNewStep::Review => "skillNew.step.review",
        }
    }
}

pub struct SkillNewState {
    pub step: SkillNewStep,
    /// Local-only gate mirroring the canvas's own muted "下一步" pill: the
    /// canvas's "確認草稿完成" text-link flips this, which then unlocks the
    /// step-2→3 nav button. No RPC involved (see this file's header
    /// comment) — a real `draft` status still requires `skills.custom_
    /// generate` to actually settle, which this page does not call.
    pub draft_confirmed: bool,
}

impl Default for SkillNewState {
    fn default() -> Self {
        // WP-S6b2-M's own "展示重點" — see header comment — is step 2
        // ("生成"), matching the canvas's own drawn state (step 1 已完成,
        // step 2 現行) rather than the flow's literal entry point.
        Self { step: SkillNewStep::Generate, draft_confirmed: false }
    }
}

impl Global for SkillNewState {}

fn go_to_step(cx: &mut Context<RootView>, step: SkillNewStep) {
    cx.global_mut::<SkillNewState>().step = step;
    cx.notify();
}

fn cancel_wizard(view: &mut RootView, cx: &mut Context<RootView>) {
    *cx.global_mut::<SkillNewState>() = SkillNewState::default();
    view.active_page = "skills";
    cx.notify();
}

// ── Breadcrumb + header ───────────────────────────────────────────────

fn breadcrumb(locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    div()
        .id("skillnew-breadcrumb")
        .flex()
        .items_center()
        .gap_1p5()
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(
            div()
                .id("skillnew-breadcrumb-root")
                .cursor_pointer()
                .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
                .child(i18n::t(locale, "skillNew.breadcrumb.skills"))
                .on_click(cx.listener(|this, _ev, _window, cx| {
                    this.active_page = "skills";
                    cx.notify();
                })),
        )
        .child(SharedString::from("›"))
        .child(div().overflow_hidden().child(i18n::t(locale, "skillNew.title")))
}

fn header(locale: Locale, cx: &mut Context<RootView>) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().text_size(px(theme::TEXT_XL)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "skillNew.title")))
                .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "skillNew.subtitle"))),
        )
        .child(button("skillnew-cancel", i18n::t(locale, "skillNew.cancel"), ButtonVariant::Secondary, false, None, cx.listener(|this, _ev, _window, cx| cancel_wizard(this, cx))))
}

// ── Stepper (4 dots, freely navigable — pure local UI state) ─────────────

fn stepper(locale: Locale, current: SkillNewStep, cx: &mut Context<RootView>) -> Div {
    let mut row = div().flex().items_center().gap_3p5();
    for (i, step) in STEPS.iter().copied().enumerate() {
        let done = step.ordinal() < current.ordinal();
        let active = step == current;
        let marker = if done {
            div()
                .size(px(22.))
                .flex_shrink_0()
                .rounded_full()
                .bg(theme::alpha(theme::SUCCESS, 0.14))
                .text_color(theme::alpha(theme::SUCCESS, 1.0))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(11.))
                .font_weight(gpui::FontWeight::BOLD)
                .child("✓")
        } else if active {
            div()
                .size(px(22.))
                .flex_shrink_0()
                .rounded_full()
                .bg(theme::alpha(theme::BRAND, 1.0))
                .text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(11.))
                .font_weight(gpui::FontWeight::BOLD)
                .child((i + 1).to_string())
        } else {
            div()
                .size(px(22.))
                .flex_shrink_0()
                .rounded_full()
                .bg(theme::alpha(theme::MUTED, 1.0))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(11.))
                .font_weight(gpui::FontWeight::BOLD)
                .child((i + 1).to_string())
        };
        let label_color = if active { theme::FOREGROUND } else { theme::MUTED_FOREGROUND };
        let mut item = div()
            .id(SharedString::from(format!("skillnew-step-{i}")))
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .child(marker)
            .child(
                div()
                    .text_size(px(11.5))
                    .font_weight(if active { gpui::FontWeight::SEMIBOLD } else { gpui::FontWeight::MEDIUM })
                    .text_color(theme::alpha(label_color, if active { 1.0 } else { 0.85 }))
                    .child(i18n::t(locale, step.title_key())),
            )
            .on_click(cx.listener(move |_this, _ev, _window, cx| go_to_step(cx, step)));
        if i + 1 < STEPS.len() {
            item = item.child(div().w(px(28.)).h(px(1.)).bg(theme::alpha(theme::MUTED, 1.0)));
        }
        row = row.child(item);
    }
    row
}

// ── Step card shell (shared padding/border across all four steps) ───────

fn step_card(child: impl IntoElement) -> Div {
    div()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .p_5()
        .flex()
        .flex_col()
        .gap_4()
        .child(child)
}

// ── Step 1: describe (no canvas reference — minimal honest stub) ────────

fn describe_view(locale: Locale) -> Div {
    step_card(
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_size(px(11.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "skillNew.describe.label")))
                    .child(
                        div()
                            .rounded(px(theme::RADIUS_LG))
                            .border_1()
                            .border_color(theme::input_border())
                            .bg(theme::alpha(theme::SURFACE, 1.0))
                            .min_h(px(72.))
                            .px_3()
                            .py_2()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.8))
                            .child(i18n::t(locale, "skillNew.describe.placeholder")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "skillNew.describe.agentLabel")))
                    .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::MEDIUM).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child("Coder")),
            )
            .child(div().flex().justify_end().child(button("skillnew-start-generate", i18n::t(locale, "skillNew.describe.startGenerate"), ButtonVariant::Primary, true, None, |_ev, _window, _cx| {}))),
    )
}

// ── Step 2: generate — this task's own "展示重點" ─────────────────────

fn generate_view(locale: Locale, draft_confirmed: bool, cx: &mut Context<RootView>) -> Div {
    let headline = div()
        .flex()
        .flex_col()
        .items_center()
        .gap_1p5()
        .py_2()
        .child(div().size(px(40.)).rounded_full().bg(theme::alpha(theme::MUTED, 1.0)).flex().items_center().justify_center().text_size(px(18.)).child("🐾"))
        .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).text_center().child(i18n::t(locale, "skillNew.generate.title")))
        .child(div().max_w(px(380.)).text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).text_center().child(i18n::t(locale, "skillNew.generate.desc")));

    let draft_path = div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_size(px(11.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "skillNew.generate.draftPathLabel")))
        .child(
            div()
                .rounded(px(theme::RADIUS_MD))
                .bg(theme::alpha(theme::MUTED, 0.5))
                .px_2p5()
                .py_1p5()
                .font_family("SF Mono")
                .text_size(px(11.5))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child("skills/custom/my-new-skill/SKILL.md"),
        );

    let instruction = div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_size(px(11.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "skillNew.generate.instructionLabel")))
        .child(
            div()
                .rounded(px(theme::RADIUS_LG))
                .border_1()
                .border_color(theme::input_border())
                .bg(theme::alpha(theme::SURFACE, 1.0))
                .min_h(px(44.))
                .px_3()
                .py_2()
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.8))
                .child(i18n::t(locale, "skillNew.generate.instructionPlaceholder")),
        );

    let confirm_row = div()
        .flex()
        .items_center()
        .gap_2()
        .pb_3()
        .border_b_1()
        .border_color(theme::border())
        .child(button("skillnew-regenerate", i18n::t(locale, "skillNew.action.regenerate"), ButtonVariant::Secondary, true, None, |_ev, _window, _cx| {}))
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "skillNew.action.or")))
        .child(
            div()
                .id("skillnew-confirm-draft")
                .text_size(px(theme::TEXT_XS))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::alpha(theme::BRAND, 1.0))
                .cursor_pointer()
                .hover(|s| s.text_color(theme::alpha(theme::BRAND, 0.85)))
                .child(i18n::t(locale, "skillNew.action.confirmDraft"))
                .on_click(cx.listener(|_this, _ev, _window, cx| {
                    cx.global_mut::<SkillNewState>().draft_confirmed = true;
                    cx.notify();
                })),
        );

    let nav_row = div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .id("skillnew-back-to-describe")
                .flex()
                .items_center()
                .gap_1p5()
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .cursor_pointer()
                .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
                .child("←")
                .child(i18n::t(locale, "skillNew.action.back"))
                .on_click(cx.listener(|_this, _ev, _window, cx| go_to_step(cx, SkillNewStep::Describe))),
        )
        .child(button(
            "skillnew-next-form",
            i18n::t(locale, "skillNew.action.nextForm"),
            ButtonVariant::Primary,
            !draft_confirmed,
            None,
            cx.listener(|_this, _ev, _window, cx| go_to_step(cx, SkillNewStep::FormFields)),
        ));

    step_card(div().flex().flex_col().gap_4().child(headline).child(draft_path).child(instruction).child(confirm_row).child(nav_row))
}

// ── Step 3/4: no canvas reference — minimal honest stubs ─────────────────

fn form_view(locale: Locale, cx: &mut Context<RootView>) -> Div {
    let field = |label_key: &'static str, value: &'static str| {
        div()
            .flex()
            .items_center()
            .justify_between()
            .py_2()
            .border_b_1()
            .border_color(theme::border())
            .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, label_key)))
            .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(value))
    };

    let nav_row = div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .id("skillnew-back-to-generate")
                .flex()
                .items_center()
                .gap_1p5()
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .cursor_pointer()
                .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
                .child("←")
                .child(i18n::t(locale, "skillNew.action.back"))
                .on_click(cx.listener(|_this, _ev, _window, cx| go_to_step(cx, SkillNewStep::Generate))),
        )
        .child(button(
            "skillnew-next-review",
            i18n::t(locale, "skillNew.action.nextReview"),
            ButtonVariant::Primary,
            false,
            None,
            cx.listener(|_this, _ev, _window, cx| go_to_step(cx, SkillNewStep::Review)),
        ));

    step_card(
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(field("skillNew.form.displayNameLabel", "reply-digest-summarizer"))
            .child(field("skillNew.form.descriptionLabel", "—"))
            .child(field("skillNew.form.timeSavedLabel", "30 分鐘 / 次"))
            .child(div().pt_3().child(nav_row)),
    )
}

fn review_view(locale: Locale, cx: &mut Context<RootView>) -> Div {
    let nav_row = div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .id("skillnew-back-to-form")
                .flex()
                .items_center()
                .gap_1p5()
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .cursor_pointer()
                .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
                .child("←")
                .child(i18n::t(locale, "skillNew.action.back"))
                .on_click(cx.listener(|_this, _ev, _window, cx| go_to_step(cx, SkillNewStep::FormFields))),
        )
        .child(button("skillnew-submit", i18n::t(locale, "skillNew.action.submit"), ButtonVariant::Primary, true, None, |_ev, _window, _cx| {}));

    step_card(
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "skillNew.review.desc")))
            .child(nav_row),
    )
}

// ── Top-level render ──────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    cx.default_global::<SkillNewState>();
    let locale = state.locale;
    let (step, draft_confirmed) = {
        let st = cx.global::<SkillNewState>();
        (st.step, st.draft_confirmed)
    };

    let content = match step {
        SkillNewStep::Describe => describe_view(locale),
        SkillNewStep::Generate => generate_view(locale, draft_confirmed, cx),
        SkillNewStep::FormFields => form_view(locale, cx),
        SkillNewStep::Review => review_view(locale, cx),
    };

    div()
        .id("skillnew-page")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .items_center()
        .child(
            div()
                .w_full()
                .max_w(px(CONTENT_MAX_WIDTH))
                .p_6()
                .flex()
                .flex_col()
                .gap_4()
                .child(breadcrumb(locale, cx))
                .child(header(locale, cx))
                .child(stepper(locale, step, cx))
                .child(content),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_new_step_ordinal_orders_the_four_steps() {
        assert_eq!(SkillNewStep::Describe.ordinal(), 0);
        assert_eq!(SkillNewStep::Generate.ordinal(), 1);
        assert_eq!(SkillNewStep::FormFields.ordinal(), 2);
        assert_eq!(SkillNewStep::Review.ordinal(), 3);
    }

    #[test]
    fn default_state_demos_the_generate_step_per_this_task_own_brief() {
        let st = SkillNewState::default();
        assert_eq!(st.step, SkillNewStep::Generate);
        assert!(!st.draft_confirmed);
    }
}
