// Y20-P2 (2026-08-29) — the live-installer wizard's own chrome (background +
// bottom nav). Renders the full-screen installer surface for whatever step
// `flow` is currently on — the shell root's ENTIRE child while a live-boot
// install session is active, same "no app chrome underneath at all" shape
// `oobe/render.rs`'s own header comment establishes for OOBE (see
// `main.rs`'s render-root if-else chain, where this branch is checked
// FIRST, ahead of the OOBE branch).
//
// Narrower than `oobe/render.rs`'s own frame in three ways, all because this
// flow has no equivalent need yet:
// - No Skip button — none of `LiveInstallStep::ALL` is skippable at P2
//   (`DiskSelect`/`Confirm`/`Progress` all need a real answer before install
//   can proceed, once P3 wires that up; skipping straight to "write disk"
//   would be actively dangerous, not a convenience).
// - No wired-network special case — `OobeFlow::can_advance_with_wired`
//   exists because OOBE's `Network` step has an environmental fact (a cable
//   plugged in) that can satisfy its precondition without a persisted
//   selection; nothing on this wizard has an analogous case.
// - No Finish-labeled terminal button — `Progress` isn't "done", it's
//   "P3 hasn't wired up what happens after a real write finishes yet"; using
//   OOBE's `Key::NavGetStarted` wording here would overstate what P2
//   actually does.
//
// Reuses `oobe::widgets`' `card`/`step_button`/`progress_dots`/`title`/
// `subtitle` (promoted `pub(super)` -> `pub(crate)`, see that file's own
// header comment) rather than re-deriving near-identical helpers.

use gpui::{div, prelude::*, px, Context, Div, Stateful};

use duduclaw_native_gui::theme;

use super::steps;
use super::{LiveInstallFlow, LiveInstallStep};
use crate::i18n::{t, Key};
use crate::oobe::widgets::{self, StepButtonVariant};
use crate::ShellView;

pub(crate) fn render(flow: &LiveInstallFlow, cx: &mut Context<ShellView>) -> Stateful<Div> {
    let step = flow.current();
    let palette = flow.palette();
    // Same "publish the ambient `ShellPalette` global BEFORE any surface
    // renders" convention `oobe::render::render` establishes — `widgets::
    // OobeTextField` (not used by this flow at P2, but shared crate-wide)
    // reads it from `cx` rather than a render parameter. Harmless no-op for
    // every widget THIS flow actually renders this round.
    cx.set_global(palette);

    div()
        .id("live-install-root")
        .size_full()
        .flex()
        .flex_col()
        // Same plain warm-neutral canvas OOBE uses, for the same reason:
        // visually distinct from Home's chrome, which this session never
        // even constructs (see `main.rs`'s render-root branch order).
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
                .child(div().w(px(640.)).flex().flex_col().gap(px(20.)).child(steps::render(step, flow, cx))),
        )
        .child(button_row(step, flow, cx))
}

fn button_row(step: LiveInstallStep, flow: &LiveInstallFlow, cx: &mut Context<ShellView>) -> Div {
    let can_advance = flow.can_advance();
    let can_back = step != LiveInstallStep::Language;
    let locale = flow.locale();
    let palette = flow.palette();

    // Each click handler re-borrows `view.live_install` fresh at invocation
    // time, same "don't capture stale state at render time" discipline
    // `oobe/render.rs`'s own `button_row` follows.
    let back_click = cx.listener(|view, _ev, _window, cx| {
        if let Some(flow) = view.live_install.as_mut() {
            flow.back();
        }
        cx.notify();
    });
    let continue_click = cx.listener(|view, _ev, _window, cx| {
        if let Some(flow) = view.live_install.as_mut() {
            flow.next();
        }
        cx.notify();
    });

    // Same three-column layout `oobe/render.rs`'s `NAV_COLUMN_WIDTH` uses —
    // fixed-width outer columns keep the progress dots centered on the
    // screen regardless of button width.
    const NAV_COLUMN_WIDTH: f32 = 180.;

    let left = div().w(px(NAV_COLUMN_WIDTH)).when(can_back, |el| {
        el.child(widgets::step_button("live-install-back", t(locale, Key::NavBack), StepButtonVariant::Ghost, false, palette, back_click))
    });

    let right = div().w(px(NAV_COLUMN_WIDTH)).flex().items_center().justify_end().child(widgets::step_button(
        "live-install-continue",
        t(locale, Key::NavContinue),
        StepButtonVariant::Primary,
        !can_advance,
        palette,
        continue_click,
    ));

    div()
        .w_full()
        .flex()
        .items_center()
        .px(px(48.))
        .py(px(20.))
        .border_t_1()
        .border_color(palette.border())
        .child(left)
        .child(div().flex_1().child(widgets::progress_dots(step.index(), LiveInstallStep::ALL.len(), palette)))
        .child(right)
}
