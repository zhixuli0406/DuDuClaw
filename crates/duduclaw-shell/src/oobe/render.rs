// OOBE frame — Shell-S1.
//
// Renders the full-screen OOBE surface for whatever step `flow` is
// currently on. This is the shell root's ENTIRE child while OOBE is active
// — no Home chrome underneath at all (task brief: "OOBE 滿版...無任何殼
// chrome"). `main.rs`'s `Render::render` picks this branch INSTEAD OF
// `home::render(cx)` (not layered as an overlay on top of it) — the
// cleaner of the two options the task brief offered ("若以疊層方式掛在
// root 上必須 absolute inset_0（或直接取代 root 內容——擇乾淨者）"): since
// this is simply the root's one flow child, same slot Home normally
// occupies, there is no stacking to get wrong and `overlay.rs`'s own P8f
// "absolute().inset_0() or it lays out one window-height offscreen" lesson
// doesn't even apply here.
//
// Layout: a persistent bottom nav bar (`button_row`) pinned to the window's
// bottom edge, with the step's own title/subtitle/content vertically
// centered above it — the shared "步驟頁模板（大標/說明/內容區/底部按鈕
// 列）" the task brief asks for, split so every step file
// (`steps/*.rs`) only owns its own content area, never the chrome around
// it.

use gpui::{div, prelude::*, px, Context, Stateful, Div};

use duduclaw_native_gui::theme;

use super::widgets::{self, AccountFields, StepButtonVariant};
use super::{steps, OobeFlow, OobeStep, OobeUiState};
use crate::i18n::{t, Key};
use crate::ShellView;

pub fn render(flow: &OobeFlow, ui: &OobeUiState, account_fields: &AccountFields, cx: &mut Context<ShellView>) -> Stateful<Div> {
    let step = flow.current();
    let palette = flow.palette();
    // Ambient theme for `widgets::OobeTextField` (the ONE OOBE widget that
    // can't take `palette` as a normal render parameter — see that struct's
    // own doc comment in `widgets.rs` for why). Set BEFORE the tree below is
    // built, so it's already current by the time that entity's own render
    // pass runs later in this SAME frame — see `palette.rs`'s own header
    // comment ("`Global` — why `OobeTextField` needs it") for the fuller
    // reasoning.
    cx.set_global(palette);

    div()
        .id("oobe-root")
        .size_full()
        .flex()
        .flex_col()
        // A plain warm-neutral canvas, deliberately NOT `home.rs`'s
        // wallpaper gradient — every surveyed OS keeps OOBE visually
        // separate from its desktop chrome (§B-3 point 7: "OOBE 必須獨立
        // 滿版舞台，任何 app chrome 不得出現"). Theme-aware as of the `Theme`
        // step (2026-08-20): resolves through `flow.palette()`, not a fixed
        // `theme::light::*` reference — see `OobeStep::Theme`'s own doc
        // comment in `oobe/mod.rs` for why picking a theme has to retheme
        // this whole surface, including this outermost background.
        .bg(theme::alpha(palette.app_shell, 1.0))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(28.))
                .px(px(48.))
                .child(widgets::progress_dots(step.index(), OobeStep::ALL.len(), palette))
                // No `.items_center()` here, deliberately (2026-08-20 user
                // report "把所有版面寬度統一"): centering THIS wrapper made
                // each step's root shrink to its intrinsic content width, so
                // every card's `w_full` resolved against a different width
                // per step (Update narrow, Privacy wide…). Flex-column
                // default align (stretch) pins every step root to the full
                // 640px, so all cards render one uniform width — matching
                // the approved design boards, where every card is the full
                // 640 column. Title/subtitle centering is each step root's
                // own `.items_center()`, not this wrapper's job.
                .child(div().w(px(640.)).flex().flex_col().gap(px(20.)).child(steps::render(step, flow, ui, account_fields, cx))),
        )
        .child(button_row(step, flow, cx))
}

fn button_row(step: OobeStep, flow: &OobeFlow, cx: &mut Context<ShellView>) -> Div {
    let can_advance = flow.can_advance();
    // Task brief item 1: language is now the first step, so IT is the one
    // with no Back — see `OobeFlow::back()`'s own guard in `oobe/mod.rs`.
    let can_back = step != OobeStep::LanguageAccessibility;
    let skippable = step.is_skippable();
    let is_finish = step == OobeStep::Finish;
    let locale = flow.locale();
    let palette = flow.palette();

    // Each click handler re-borrows `view.oobe` fresh at invocation time
    // (not at build time) — the closures below just call the same
    // `OobeFlow` methods + `super::save_state` the keyboard bindings in
    // `main.rs` (`on_oobe_next` / `on_close_overlay`'s OOBE branch) call,
    // so mouse and keyboard navigation always agree.
    let back_click = cx.listener(|view, _ev, _window, cx| {
        if let Some(flow) = view.oobe.as_mut() {
            flow.back();
            super::save_state(flow.state());
        }
        cx.notify();
    });
    let skip_click = cx.listener(|view, _ev, _window, cx| {
        if let Some(flow) = view.oobe.as_mut() {
            flow.skip();
            super::save_state(flow.state());
            if flow.completed() {
                view.oobe = None;
            }
        }
        cx.notify();
    });
    let continue_click = cx.listener(|view, _ev, _window, cx| {
        if let Some(flow) = view.oobe.as_mut() {
            flow.next();
            super::save_state(flow.state());
            if flow.completed() {
                view.oobe = None;
            }
        }
        cx.notify();
    });

    let left = div().when(can_back, |el| {
        el.child(widgets::step_button("oobe-back", t(locale, Key::NavBack), StepButtonVariant::Ghost, false, palette, back_click))
    });

    let right = div()
        .flex()
        .items_center()
        .gap(px(10.))
        .when(skippable, |el| {
            el.child(widgets::step_button("oobe-skip", t(locale, Key::NavSkip), StepButtonVariant::Secondary, false, palette, skip_click))
        })
        .child(widgets::step_button(
            "oobe-continue",
            // Task brief step 8: 「開始使用」→ completed=true 存檔 → 進
            // Home" — this SAME button (no separate in-card duplicate; see
            // `steps/finish.rs`'s own header comment) is what fires that
            // transition for the Finish step, so its label is the one the
            // brief names, not round 1's generic "完成".
            if is_finish { t(locale, Key::NavGetStarted) } else { t(locale, Key::NavContinue) },
            StepButtonVariant::Primary,
            !can_advance,
            palette,
            continue_click,
        ));

    div()
        .w_full()
        .flex()
        .items_center()
        .justify_between()
        .px(px(48.))
        .py(px(20.))
        .border_t_1()
        .border_color(palette.border())
        .child(left)
        .child(right)
}
