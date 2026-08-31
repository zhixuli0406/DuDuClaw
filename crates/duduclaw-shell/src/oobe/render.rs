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

use super::widgets::{self, AccountFields, NetworkFields, StepButtonVariant};
use super::{steps, OobeFlow, OobeStep, OobeUiState};
use crate::i18n::{t, Key};
use crate::ShellView;

pub fn render(flow: &OobeFlow, ui: &OobeUiState, account_fields: &AccountFields, network_fields: &NetworkFields, cx: &mut Context<ShellView>) -> Stateful<Div> {
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
                // ICON-3 (2026-08-23): the progress dots used to be the
                // FIRST child here, above the step's own title. The board
                // moves them into the bottom toolbar alongside 返回/繼續 —
                // see `widgets::progress_dots`'s own doc comment for the
                // measurements and `button_row` below for the placement.
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
                .child(div().w(px(640.)).flex().flex_col().gap(px(20.)).child(steps::render(step, flow, ui, account_fields, network_fields, cx))),
        )
        .child(button_row(step, flow, ui, cx))
}

/// D4a-5 (2026-08-23): `ui` is threaded in only so this fn can read
/// `ui.wired_online()` for the `Network` step's Continue-button gate — see
/// `OobeFlow::can_advance_with_wired`'s own doc comment for why that check
/// lives on a SEPARATE method from `can_advance()` rather than widening
/// this fn's existing `flow.can_advance()` call silently.
fn button_row(step: OobeStep, flow: &OobeFlow, ui: &OobeUiState, cx: &mut Context<ShellView>) -> Div {
    let can_advance = flow.can_advance_with_wired(ui.wired_online());
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
    // D2-b (2026-08-24): the mouse-driven completion paths below used to
    // set `view.oobe = None` on `flow.completed()` WITHOUT the two lines
    // `main.rs`'s keyboard path (`handle_enter_key`'s `EnterOutcome::
    // Advance` arm) carries out right next to its own `self.oobe = None` —
    // copying `flow.state().selections.theme` onto `view.theme` and telling
    // comp via `notify_comp_theme`. `ShellView::render` paints Home from
    // `self.theme`, never from the (now-gone) `flow.state()`, so OOBE
    // completed via 完成/略過 (the buttons everyone actually clicks —
    // `Enter` is a keyboard-only alternate path) landed on Home still
    // showing whatever `initial_theme` boot loaded, even though `save_
    // state` had already written the picked theme to disk correctly (which
    // is why a SHELL RESTART after this same OOBE run picked it up fine —
    // only the same-process transition was missing it). Both closures below
    // now mirror `main.rs`'s two lines exactly, so all three completion
    // sites (Enter, 完成, 略過) agree.
    let skip_click = cx.listener(|view, _ev, _window, cx| {
        if let Some(flow) = view.oobe.as_mut() {
            flow.skip();
            super::save_state(flow.state());
            if flow.completed() {
                view.theme = flow.state().selections.theme;
                crate::notify_comp_theme(view.theme);
                view.oobe = None;
            }
        }
        cx.notify();
    });
    // D4a-7 (2026-08-31, QEMU wired-only OOBE deadlock): this closure now
    // handles TWO more outcomes beyond "advanced"/"completed" — see `oobe/
    // steps/network.rs`'s own header comment ("D4a-7") for the full root
    // cause and fix shape this backs. Both new branches call OUT to
    // `steps::on_network_step_entered`/`steps::on_network_continue_blocked`
    // (thin `pub(super)` bridges in `steps/mod.rs`) AFTER the `if let Some
    // (flow) = view.oobe.as_mut()` block below fully closes — `flow`'s
    // borrow of `view.oobe` has already ended by then (its last use is
    // inside that block), so these whole-`view` calls need no further
    // borrow-splitting.
    let continue_click = cx.listener(|view, _ev, _window, cx| {
        // D4a-5: read fresh at click time (not captured at render time) —
        // same "re-borrow `view.oobe`/`view.oobe_ui` at invocation" pattern
        // this closure already follows for `view.oobe`, see this fn's own
        // header comment.
        let wired_online = view.oobe_ui.wired_online();
        let mut entered_network = false;
        let mut blocked_on_network = false;
        if let Some(flow) = view.oobe.as_mut() {
            let step_before = flow.current();
            let advanced = flow.next_with_wired(wired_online);
            super::save_state(flow.state());
            if advanced {
                if flow.completed() {
                    // D2-b: see this fn's header comment just above `skip_
                    // click` for why these two lines must run here too.
                    view.theme = flow.state().selections.theme;
                    crate::notify_comp_theme(view.theme);
                    view.oobe = None;
                } else if flow.current() == OobeStep::Network {
                    // Entered `Network` from the PREVIOUS step's own
                    // Continue click — still click-triggered I/O (the
                    // operator's own click, just landing on the NEXT
                    // step), not a render-time side effect.
                    entered_network = true;
                }
            } else if step_before == OobeStep::Network {
                // Blocked ON `Network`'s own precondition. Only reachable
                // at all because `button_row`'s Continue button below is
                // built through `widgets::step_button_ex` with `clickable_
                // while_disabled: true` for exactly this step — every other
                // step's disabled Continue never attaches `on_click` at
                // all, so this closure can't even fire for them while
                // blocked.
                blocked_on_network = true;
            }
        }
        if entered_network {
            steps::on_network_step_entered(view, cx);
        } else if blocked_on_network {
            steps::on_network_continue_blocked(view, cx);
        }
        cx.notify();
    });

    // ICON-3 (2026-08-23): the toolbar is now a THREE-column row —
    // `OOBE-ProgressAndIcons.dc.html` fixes the outer two at 180px each and
    // lets the dots take the flexible middle, so the dot group stays
    // centred on the SCREEN rather than drifting with the button widths
    // (which change with the locale and with whether 略過 is present).
    // `justify_between` was enough for two columns; it is not for three.
    const NAV_COLUMN_WIDTH: f32 = 180.;

    let left = div().w(px(NAV_COLUMN_WIDTH)).when(can_back, |el| {
        el.child(widgets::step_button("oobe-back", t(locale, Key::NavBack), StepButtonVariant::Ghost, false, palette, back_click))
    });

    let right = div()
        .w(px(NAV_COLUMN_WIDTH))
        .flex()
        .items_center()
        .justify_end()
        .gap(px(10.))
        .when(skippable, |el| {
            el.child(widgets::step_button("oobe-skip", t(locale, Key::NavSkip), StepButtonVariant::Secondary, false, palette, skip_click))
        })
        .child(widgets::step_button_ex(
            "oobe-continue",
            // Task brief step 8: 「開始使用」→ completed=true 存檔 → 進
            // Home" — this SAME button (no separate in-card duplicate; see
            // `steps/finish.rs`'s own header comment) is what fires that
            // transition for the Finish step, so its label is the one the
            // brief names, not round 1's generic "完成".
            if is_finish { t(locale, Key::NavGetStarted) } else { t(locale, Key::NavContinue) },
            StepButtonVariant::Primary,
            !can_advance,
            // D4a-7 (2026-08-31): ONLY the `Network` step keeps its
            // Continue button click-attached while disabled — see `widgets
            // ::step_button_ex`'s own doc comment for the full argument.
            // Every other step's disabled Continue stays exactly as inert
            // as plain `step_button` always made it.
            step == OobeStep::Network,
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
        .child(div().flex_1().child(widgets::progress_dots(step.index(), OobeStep::ALL.len(), palette)))
        .child(right)
}

#[cfg(test)]
mod tests {
    /// D2-b (2026-08-24): both mouse-driven completion closures in
    /// [`button_row`] (`continue_click` for 完成/繼續, `skip_click` for 略過)
    /// must carry `flow.state().selections.theme` onto `view.theme` and call
    /// `notify_comp_theme` before dropping `view.oobe` — see `ShellView::
    /// theme`'s own doc comment (`main.rs`) for why. `Enter` (`main.rs`'s
    /// `handle_enter_key`) already did this; the two button closures here did
    /// not, which is exactly why a same-process OOBE→Home transition via the
    /// button everyone actually clicks silently kept the boot-time theme
    /// while a shell RESTART after the same run picked the persisted choice
    /// up fine (`oobe::save_state` was never the missing half).
    ///
    /// Same "crude but load-bearing" source-scan shape `main.rs`'s own test
    /// module already uses for gpui closures a plain unit test cannot drive
    /// (no `TestAppContext` window round-trip for one assertion) — it cannot
    /// prove the theme reaches the screen (that's the VM live check), but it
    /// fails loudly the instant either closure regresses to bare
    /// `view.oobe = None`.
    #[test]
    fn both_oobe_completion_buttons_carry_the_theme_pick_onto_home() {
        let source = include_str!("render.rs");
        let closures = [
            ("skip_click", "let skip_click ="),
            ("continue_click", "let continue_click ="),
        ];
        for (name, marker) in closures {
            let start = source.find(marker).unwrap_or_else(|| panic!("{name} closure not found in oobe/render.rs"));
            // Each closure body is short and self-contained; a fixed forward
            // window comfortably covers it without needing a real brace
            // matcher (same pragmatism the rest of this crate's source-scan
            // tests use).
            let window = &source[start..(start + 1200).min(source.len())];
            assert!(
                window.contains("view.theme = flow.state().selections.theme"),
                "{name} completes OOBE without carrying the theme pick onto `view.theme` — \
                 Home would keep showing the boot-time theme until the next restart"
            );
            assert!(
                window.contains("notify_comp_theme(view.theme)"),
                "{name} completes OOBE without telling comp the new theme via notify_comp_theme"
            );
        }
    }

    // ── D4a-7 (2026-08-31): the wired-only-machine Continue deadlock fix ──
    // Same source-scan shape as the theme test above — see its own doc
    // comment for why (no `TestAppContext` window round-trip for these
    // assertions). These two lock the two halves of the fix independently:
    // the button must stay click-attached while disabled ONLY for
    // `Network`, and the click handler must actually reach both new hooks.

    #[test]
    fn the_continue_button_is_click_attached_while_disabled_only_for_the_network_step() {
        let source = include_str!("render.rs");
        let start = source
            .find("widgets::step_button_ex(\n            \"oobe-continue\",")
            .unwrap_or_else(|| panic!("oobe-continue must be built via widgets::step_button_ex(...) in oobe/render.rs — \
                 plain widgets::step_button never attaches on_click while disabled, which is \
                 exactly the QEMU wired-only deadlock (a blocked click on Network must still fire)"));
        let window = &source[start..(start + 1100).min(source.len())];
        assert!(
            window.contains("step == OobeStep::Network,"),
            "the Continue button's `clickable_while_disabled` argument must be `step == \
             OobeStep::Network` — every OTHER step's disabled Continue must stay fully inert, \
             unchanged from before this fix"
        );
    }

    #[test]
    fn continue_click_reaches_both_network_step_hooks() {
        let source = include_str!("render.rs");
        let start = source.find("let continue_click =").unwrap_or_else(|| panic!("continue_click closure not found in oobe/render.rs"));
        let window = &source[start..(start + 2400).min(source.len())];
        assert!(
            window.contains("steps::on_network_step_entered(view, cx)"),
            "continue_click must trigger a scan/status recheck on entering `Network` from the \
             PREVIOUS step's Continue click — otherwise a wired-only machine still never sees \
             `wired_online()` become true until it happens to click 「掃描 Wi-Fi」 itself"
        );
        assert!(
            window.contains("steps::on_network_continue_blocked(view, cx)"),
            "continue_click must handle a BLOCKED click on `Network` itself — otherwise the \
             `step_button_ex` widening above lets the click through with nothing to catch it, \
             same dead-end UX as before, just one click later"
        );
    }
}
