// Installer-settings-integration WP1 (2026-08-29,
// `commercial/docs/DESIGN-installer-settings-integration-2026-08.md` §3.1/
// §4) — the live-installer's own Account step. Visually mirrors `oobe::
// steps::account` (title/subtitle, two labeled `OobeTextField`s, an
// unconditionally-present status slot) but is a THINNER "UI glue" layer over
// a completely different backend contract — see `LiveInstallStep::Account`'s
// own doc comment in `state.rs` for the full reasoning; the short version:
// this step is pure data collection, nothing else.
//
// ── What this step does NOT do, and why (read before "fixing" it) ─────────
// Unlike `oobe::steps::account::try_submit`, this step has:
//   - NO "建立帳號" submit button of its own. `LiveInstallFlow::set_account`
//     is called from `render.rs`'s shared bottom-nav Continue click instead
//     (click-time validation lives THERE — see that file's own
//     `validate_and_set_account`, and its header comment for why it can't
//     be a live-content-driven `disabled` state: the parent `ShellView`
//     isn't subscribed to `live_install_account_fields`'s own child-entity
//     `cx.notify()`, same reasoning `oobe::steps::account::try_submit`'s own
//     header comment gives). This flow has no Enter-key routing at all (see
//     `state.rs`'s own header comment) — unlike OOBE, there is no SECOND
//     caller (a keyboard path) that would justify extracting a shared
//     `try_submit`-shaped fn; one bottom-nav button is already this step's
//     only forward action, so a second, card-owned button would just be two
//     differently-wired buttons on one screen — the same reasoning
//     `steps::confirm`'s own header comment gives for not adding a
//     card-owned "開始安裝" button next to the shared Continue.
//   - NO network I/O of any kind. `oobe::claim::create_account` round-trips
//     to a real gateway at `127.0.0.1:18789` because OOBE always runs
//     POST-boot, with a live gateway already up. A live-install session has
//     no gateway to call (the live image "carries no gateway payload" — see
//     the design doc's own §4) — this step's whole job is to hold the typed
//     name/password in `LiveInstallState` until a LATER round
//     (`install_runner`, not yet wired this round) serializes them into a
//     `pending-account.json` the TARGET system's own first-boot gateway
//     claims. So there is no `InFlight`/background-thread/`cx.spawn` poll
//     loop here at all — every write to `LiveInstallFlow` this step's
//     collaborators make is a synchronous, local-only state write.
//
// ── Known limitation: no auto-focus, no Tab cycling (disclosed) ───────────
// OOBE's `AccountCreate` step gets Tab/Shift-Tab field cycling via
// `OobeFocusTarget`/`main.rs`'s `cycle_oobe_focus` — but that machinery is
// pure keyboard-CYCLING, not auto-focus-on-step-entry (confirmed by reading
// it end to end: nothing in this crate calls `window.focus(...)`
// automatically when a step becomes active either, so there was no cheap
// "auto-focus on arrival" precedent here to mirror). Wiring `live_install`
// into OOBE's own Tab-routing state (which is keyed off `self.oobe`/
// `OobeStep`, not this flow) — or building an equivalent from scratch — is
// keyboard-action routing this round's own scope explicitly excludes (see
// `state.rs`'s and `render.rs`'s own header comments on why this wizard is
// mouse-only for now). Left as a disclosed limitation, not silently
// dropped: the operator must click into the name field before typing, same
// as every other click-driven control in this wizard.

use gpui::{div, prelude::*, px, Context, Div, Entity};

use duduclaw_native_gui::theme;

use crate::i18n::{t, Key, Locale};
use crate::oobe::widgets::{self, AccountFields, OobeTextField};
use crate::palette::ShellPalette;
use crate::ShellView;

use super::super::{AccountError, LiveInstallFlow};

pub(super) fn render(flow: &LiveInstallFlow, fields: &AccountFields, cx: &mut Context<ShellView>) -> Div {
    let locale = flow.locale();
    let palette = flow.palette();

    let body = div()
        .flex()
        .flex_col()
        .gap(px(14.))
        .child(labeled_field(t(locale, Key::AccountNameLabel), fields.name.clone(), palette))
        .child(labeled_field(t(locale, Key::AccountPasswordLabel), fields.password.clone(), palette))
        // W7-2 discipline (see this file's own header comment and
        // `status_line`'s own doc comment below): appended unconditionally,
        // as a top-level statement, never inside a conditional — an
        // omitted-when-empty status line would shift the shared bottom-nav
        // Continue button's Y position the instant an error first appears.
        .child(status_line(flow.account_error(), locale, palette));

    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(20.))
        .child(widgets::title(t(locale, Key::AccountTitle), palette))
        .child(widgets::subtitle(t(locale, Key::AccountSubtitle), palette))
        .child(widgets::card(body, palette))
}

/// Same label-over-field layout `oobe::steps::account::labeled_field`
/// establishes (re-derived, not shared — that fn is private to `oobe::
/// steps::account`).
fn labeled_field(label: &'static str, field: Entity<OobeTextField>, palette: ShellPalette) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(label))
        .child(field)
}

/// The step's status slot — unconditionally present in `render`'s `body`
/// above, same W7-2 (`OOBE-acct-stuck`, 2026-08-24) layout-stability
/// discipline `oobe::steps::account::status_line`'s own doc comment
/// documents (that VM-reproduced bug: an appear/disappear status line
/// shifted the button below it, so a resubmit click aimed at the button's
/// pre-error position could land on inert text instead). Re-derived locally
/// rather than called from `oobe::steps::account` because that module's own
/// `status_line`/`message_line`/`message_line_with_alpha` are private `fn`s
/// scoped to `oobe::steps`, and this step's status vocabulary is smaller —
/// two `AccountError` cases, no `AccountClaimState` in-flight/
/// already-claimed cases at all, since this step never talks to a gateway
/// (see this file's own header comment).
fn status_line(error: Option<AccountError>, locale: Locale, palette: ShellPalette) -> Div {
    let (text, alpha) = match error {
        Some(AccountError::EmptyFields) => (t(locale, Key::AccountValidationError), 1.0),
        Some(AccountError::PasswordTooShort) => (t(locale, Key::AccountPasswordTooShortError), 1.0),
        // No error yet — a non-breaking space placeholder reserves the SAME
        // line height a real message would (an empty string child can
        // collapse a line's height to zero, defeating the whole point of
        // reserving space).
        None => ("\u{a0}", 0.0),
    };
    div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.destructive, alpha)).child(text)
}
