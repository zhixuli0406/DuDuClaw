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
// - No Finish-labeled terminal button in P2 — `Progress` wasn't "done" yet,
//   it was "nothing wired up after a real write finishes"; see "Y20-P3"
//   below for what replaces that placeholder.
//
// Reuses `oobe::widgets`' `card`/`step_button`/`progress_dots`/`title`/
// `subtitle` (promoted `pub(super)` -> `pub(crate)`, see that file's own
// header comment) rather than re-deriving near-identical helpers.
//
// ── Y20-P3 (2026-08-29): the shared Continue slot becomes step-aware ──────
// `button_row`'s bottom-nav Continue button is now the ONE forward action
// for every step, including the two steps that used to be inert placeholders:
//   - `Confirm`: relabeled "開始安裝" — clicking it validates + kicks off the
//     real `duduclaw-os-install` write (`install_runner::start_install`)
//     AND advances to `Progress`, in one click. See `confirm.rs`'s own
//     header comment for why this step deliberately has no SECOND,
//     card-owned submit button the way `oobe::steps::account`'s "建立帳號"
//     does — a destructive action gated behind exactly one clearly-labeled
//     button is the smaller, less error-prone surface.
//   - `Progress`: relabeled "重新開機" once the write reports `Done`
//     (`LiveInstallFlow::can_advance` gates the button itself); clicking it
//     spawns a real reboot (`install_runner::start_reboot`) rather than
//     advancing a step that has nowhere left to go.
// `Language`/`DiskSelect` keep the P2 behavior byte-identical (a bare
// `flow.next()`, labeled "NavContinue").
//
// ── Installer-settings-integration WP1 (2026-08-29): Account + Theme ──────
// `render`'s own signature grows a `fields: &AccountFields` parameter,
// threaded straight through to `steps::render` (see that dispatcher's own
// header comment — only `steps::account` reads it). `Theme` needs no new
// wiring here at all: it falls into the SAME bare-`flow.next()`/
// `NavContinue` arms `Language`/`DiskSelect` already use, since (like
// those two) it has nothing beyond a plain step transition to do on
// Continue — see `steps::theme`'s own header comment for why its click
// handler writes straight to `LiveInstallFlow` with no I/O.
//
// `Account` is the one genuinely new case: its Continue click can't be a
// bare `flow.next()`, because there is no earlier point in this step where
// `LiveInstallFlow::set_account` could have already run (unlike `Confirm`'s
// checkbox or `DiskSelect`'s row click, both of which record their pick
// immediately on click, well before Continue is ever pressed). So Continue
// itself is where the typed name/password get READ, validated, and
// recorded — `validate_and_set_account` below, called from the `Account`
// arm of `continue_click`'s match, mirrors `oobe::steps::account::
// try_submit`'s own click-time-validation discipline (see that fn's own
// header comment for why a live-content-driven `disabled` state can't
// substitute: the parent `ShellView` isn't subscribed to `live_install_
// account_fields`'s own child-entity `cx.notify()`). Because validation can
// only happen at click time, this step's Continue button is ALSO the one
// case where `can_back`'s sibling, the disabled/enabled state, does NOT
// come from `LiveInstallFlow::can_advance()` directly — see `button_row`'s
// own `continue_disabled` for why.

use gpui::{div, prelude::*, px, Context, Div, Stateful};

use duduclaw_native_gui::theme;

use super::steps;
use super::{AccountError, LiveInstallFlow, LiveInstallStep};
use crate::i18n::{t, Key};
use crate::oobe::widgets::{self, AccountFields, StepButtonVariant};
use crate::ShellView;

pub(crate) fn render(flow: &LiveInstallFlow, fields: &AccountFields, cx: &mut Context<ShellView>) -> Stateful<Div> {
    let step = flow.current();
    let palette = flow.palette();
    // Same "publish the ambient `ShellPalette` global BEFORE any surface
    // renders" convention `oobe::render::render` establishes — `widgets::
    // OobeTextField` (used by this flow's own `Account` step as of WP1, and
    // shared crate-wide) reads it from `cx` rather than a render parameter.
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
                .child(div().w(px(640.)).flex().flex_col().gap(px(20.)).child(steps::render(step, flow, fields, cx))),
        )
        .child(button_row(step, flow, cx))
}

fn button_row(step: LiveInstallStep, flow: &LiveInstallFlow, cx: &mut Context<ShellView>) -> Div {
    let can_advance = flow.can_advance();
    // Y20-P3: Progress is a dead end EXCEPT after a failed write — a failed
    // `duduclaw-os-install` run needs a way back to `Confirm` to retry (a
    // different disk, or the same one again); a successful/idle/running
    // install has nowhere useful to back up TO (the write is already
    // in-flight or already committed by the time this screen shows
    // anything at all — see `install_runner`'s own header comment).
    let can_back = step != LiveInstallStep::Language && (step != LiveInstallStep::Progress || flow.install_failed());
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

    // Y20-P3: Confirm/Progress no longer perform a bare `flow.next()` — see
    // this file's own header comment for why starting the destructive write
    // and rebooting need real side effects on top of the step transition,
    // not just the transition alone. WP1 adds a third such case, `Account`
    // (click-time validation — see `validate_and_set_account`'s own doc
    // comment below). `Language`/`Theme`/`DiskSelect` keep the P2 bare-
    // `flow.next()` behavior byte-identical. `step` is `Copy`, so capturing
    // it by value into this `move` closure is cheap and unambiguous at call
    // time.
    let continue_click = cx.listener(move |view, _ev, _window, cx| match step {
        LiveInstallStep::Confirm => super::install_runner::start_install(view, cx),
        LiveInstallStep::Progress => super::install_runner::start_reboot(view, cx),
        LiveInstallStep::Account => validate_and_set_account(view, cx),
        LiveInstallStep::Language | LiveInstallStep::Theme | LiveInstallStep::DiskSelect => {
            if let Some(flow) = view.live_install.as_mut() {
                flow.next();
            }
            cx.notify();
        }
    });

    // Y20-P3: the shared bottom-nav slot IS the destructive-action button on
    // Confirm and the reboot button on Progress — see this file's own
    // header comment (and `confirm.rs`'s) for why a second, step-owned
    // button was deliberately not added instead. WP1's `Account` needs no
    // special label — its Continue click is a validated submit, but it
    // reads to the operator exactly like every other forward step, unlike
    // Confirm's irreversible write or Progress's reboot.
    let continue_label: &'static str = match step {
        LiveInstallStep::Confirm => "開始安裝 · Start install",
        LiveInstallStep::Progress => "重新開機 · Reboot",
        LiveInstallStep::Language | LiveInstallStep::Account | LiveInstallStep::Theme | LiveInstallStep::DiskSelect => {
            t(locale, Key::NavContinue)
        }
    };

    // WP1: `Account`'s Continue button is never disabled by `can_advance`
    // directly. `LiveInstallFlow::can_advance()` for `Account` reads whether
    // `set_account` has ALREADY run — which is never true before any click,
    // since nothing pre-fills the two fields. If this button read that gate
    // the same way every other step's does, it would render permanently
    // disabled on first arrival, with no other affordance able to move the
    // flow forward. Validation happens at CLICK time instead
    // (`validate_and_set_account` below) — `LiveInstallFlow::next()`'s own
    // internal `can_advance()` check stays as defense in depth (it still
    // refuses to advance if `set_account` was somehow never reached), it
    // just isn't ALSO what disables this button.
    let continue_disabled = if step == LiveInstallStep::Account { false } else { !can_advance };

    // Same three-column layout `oobe/render.rs`'s `NAV_COLUMN_WIDTH` uses —
    // fixed-width outer columns keep the progress dots centered on the
    // screen regardless of button width.
    const NAV_COLUMN_WIDTH: f32 = 180.;

    let left = div().w(px(NAV_COLUMN_WIDTH)).when(can_back, |el| {
        el.child(widgets::step_button("live-install-back", t(locale, Key::NavBack), StepButtonVariant::Ghost, false, palette, back_click))
    });

    let right = div().w(px(NAV_COLUMN_WIDTH)).flex().items_center().justify_end().child(widgets::step_button(
        "live-install-continue",
        continue_label,
        StepButtonVariant::Primary,
        continue_disabled,
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

/// The `Account` step's click-time validation — same "validate at click
/// time, not via a live-content-driven `disabled` state" discipline `oobe::
/// steps::account::try_submit`'s own header comment documents (the parent
/// `ShellView` isn't subscribed to `live_install_account_fields`'s own
/// child-entity `cx.notify()`, so a `disabled` state derived from live typed
/// content would silently go stale between keystrokes). Unlike
/// `try_submit`, this has no gateway round trip and no `InFlight` state to
/// guard re-entrancy against — see `LiveInstallStep::Account`'s own doc
/// comment in `state.rs` for why — so this is a single synchronous fn, not
/// a background-thread/`cx.spawn` bridge.
fn validate_and_set_account(view: &mut ShellView, cx: &mut Context<ShellView>) {
    let name = view.live_install_account_fields.name.read(cx).content(cx).trim().to_string();
    let password = view.live_install_account_fields.password.read(cx).content(cx);

    if name.is_empty() || password.is_empty() {
        if let Some(flow) = view.live_install.as_mut() {
            flow.set_account_error(AccountError::EmptyFields);
        }
        cx.notify();
        return;
    }

    if password.chars().count() < 8 {
        // Mirrors `handle_first_run_claim`'s own `< 8 chars` rule
        // (`duduclaw-gateway/src/server.rs`), same client-side pre-check
        // `oobe::steps::account::try_submit` runs before ever reaching a
        // gateway. This flow never reaches a gateway AT ALL (see `steps::
        // account`'s own header comment) — but the rule is the exact same
        // floor the target system's own first-boot landing enforces
        // (`duduclaw-gateway`'s `pending_account.rs`, fed by
        // `install_runner`'s pending-account file), so failing it here is an
        // honest early check, not a guess at what the eventual rule will be.
        if let Some(flow) = view.live_install.as_mut() {
            flow.set_account_error(AccountError::PasswordTooShort);
        }
        cx.notify();
        return;
    }

    if let Some(flow) = view.live_install.as_mut() {
        flow.set_account(name, password);
        flow.next();
    }
    cx.notify();
}

#[cfg(test)]
mod tests {
    // Y20-P3: guards the Confirm/Progress step-specific wiring that a gpui
    // `cx.listener` closure can't otherwise be driven from without a live
    // window — same "crude but load-bearing" source-scan shape
    // `oobe/steps/account.rs`'s own test module already uses for an
    // identical reason (see that file's own header comment).

    #[test]
    fn confirm_step_routes_the_continue_click_to_start_install() {
        let source = include_str!("render.rs");
        assert!(
            source.contains("LiveInstallStep::Confirm => super::install_runner::start_install(view, cx)"),
            "the Confirm step's bottom-nav Continue click must dispatch to install_runner::start_install \
             — a bare flow.next() here would advance to Progress without ever starting the real write"
        );
    }

    #[test]
    fn progress_step_routes_the_continue_click_to_start_reboot() {
        let source = include_str!("render.rs");
        assert!(
            source.contains("LiveInstallStep::Progress => super::install_runner::start_reboot(view, cx)"),
            "the Progress step's bottom-nav Continue click must dispatch to install_runner::start_reboot, \
             not a bare flow.next() (Progress has no next step to advance to)"
        );
    }

    /// Installer-settings-integration WP1: `Account`'s Continue click must
    /// route to click-time validation, not a bare `flow.next()` — a bare
    /// advance here would skip past the step without ever reading, or
    /// recording, the typed name/password at all.
    #[test]
    fn account_step_routes_the_continue_click_to_click_time_validation() {
        let source = include_str!("render.rs");
        assert!(
            source.contains("LiveInstallStep::Account => validate_and_set_account(view, cx)"),
            "the Account step's bottom-nav Continue click must dispatch to validate_and_set_account \
             — a bare flow.next() here would advance past Account without ever validating or \
             recording the typed name/password"
        );
    }
}
