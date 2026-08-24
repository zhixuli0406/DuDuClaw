// Step 4 — 建立操作者帳號＋密碼（一步完成）. §B-1 row 4: macOS's own
// "唯一不可跳過的身分步驟" (§1 line 12) + the structural fix for the
// bootstrap-admin two-phase WS-handshake deadlock incident (memory note
// `project_bootstrap_admin_ws_deadlock`: "must_change_password 擋 WS 握
// 手") — no "log in with a default password, then get forced to change it"
// intermediate state exists here at all; account + password are set in ONE
// step before anything else can proceed. Not skippable — see
// `OobeStep::AccountCreate`'s own doc comment.
//
// Round 2: real typing. Round 1's fields were static prefilled fake values
// (task brief evaluation this round: "duduclaw-native-gui 的 ime_input 是
// bin-private 未曝露 lib，不要改 native-gui...real-vs-stub 評估"). That
// crate's own `text_field.rs` — a ~130-line `on_key_down`-capture text
// field, deliberately smaller than zed's full `EntityInputHandler` example
// — is proof this is cheap enough to be worth doing for real rather than
// falling back to a static stub: `oobe/widgets.rs`'s `OobeTextField` is a
// re-derivation of that exact pattern (can't reuse the original directly,
// since `duduclaw-native-gui/src/lib.rs` doesn't expose `text_field` to
// this crate — only `theme`/`mds_gpui` are public there). See that struct's
// own header comment for the full evaluation.
//
// "建立帳號" validates both fields at CLICK time (`fields.name.read(cx).
// content`), not by disabling the button ahead of time from live typed
// content — the parent `ShellView` isn't subscribed to either child
// entity's `cx.notify()`, so a live-content-driven disabled state would
// silently go stale between keystrokes. This mirrors
// `duduclaw-native-gui/src/screens/login.rs`'s own submit handler, which
// reads `email_field`/`password_field` the identical way inside its own
// click listener rather than gating the button's enabled state on them.
//
// ── Shell-S2 round 1 (2026-08-20): real gateway RPC ──────────────────────
// Round 2's click handler stopped at `flow.set_account_created(true)` with
// no I/O at all — a local-only stub. This round wires it to the real
// gateway (`oobe::claim::create_account`, itself a hand-rolled HTTP/1.1
// client over `/api/first-run/status` + `/api/first-run/claim` — see that
// module's own header comment for why no `reqwest`/`tokio` dependency was
// added). The click handler now:
//   1. Guards against a click landing while a previous request is still
//      in flight (no-op).
//   2. Re-validates name/password non-empty (unchanged from round 2).
//   3. Pre-checks password length client-side (mirrors the gateway's own
//      `< 8 chars` rule) so the operator never has to round-trip just to
//      learn it.
//   4. Spawns a `std::thread` to run the blocking network call, bridging
//      its result back via `std::sync::mpsc` + a one-shot `cx.spawn` poll
//      loop — the SAME background-thread -> channel -> foreground-executor
//      pattern `duduclaw-native-gui/src/main.rs`'s own `main()` uses for its
//      persistent session/chat event channels (see that file's header
//      comment), just one-shot instead of a `loop` that runs for the
//      window's whole lifetime.
// `DUDUCLAW_SHELL_OOBE_LOCAL_ACCOUNT=1` (documented in `main.rs`'s env-var
// list) is a dev-only escape hatch that skips all of the above and
// reproduces round 2's original local-only behavior verbatim — for headless
// smoke runs with no gateway reachable.

use gpui::{div, prelude::*, px, Context, Div};

use duduclaw_native_gui::theme;

use crate::i18n::{t, Key};
use crate::oobe::widgets::{AccountFields, StepButtonVariant};
use crate::oobe::{claim, widgets, AccountClaimFailureKind, AccountClaimState, OobeFlow, OobeUiState};
use crate::ShellView;

pub(super) fn render(flow: &OobeFlow, ui: &OobeUiState, fields: &AccountFields, cx: &mut Context<ShellView>) -> Div {
    let created = flow.selections().account_created;
    let locale = flow.locale();
    let palette = flow.palette();
    let in_flight = ui.account_claim == AccountClaimState::InFlight;

    let create_click = cx.listener(move |view, _ev, _window, cx| try_submit(view, cx));

    let mut body = div()
        .flex()
        .flex_col()
        .gap(px(14.))
        .child(labeled_field(t(locale, Key::AccountNameLabel), fields.name.clone(), palette))
        .child(labeled_field(t(locale, Key::AccountPasswordLabel), fields.password.clone(), palette));

    // W7-2 (OOBE-acct-stuck, 2026-08-24): the status slot is now ALWAYS
    // appended — never conditionally — so the button below it never moves.
    // See `status_line`'s own doc comment for the VM-reproduced failure
    // mode this closes: a `message_line` appearing/disappearing used to
    // shift the "建立帳號" button's Y position, so a resubmit click aimed at
    // the button's PRE-error location could land on the (inert) message
    // text instead once an error was showing — indistinguishable from
    // `try_submit` itself refusing to run again.
    let status = if ui.account_validation_error {
        Some((t(locale, Key::AccountValidationError), palette.destructive))
    } else {
        match ui.account_claim {
            AccountClaimState::Failed(AccountClaimFailureKind::PasswordTooShort) => {
                Some((t(locale, Key::AccountPasswordTooShortError), palette.destructive))
            }
            AccountClaimState::Failed(AccountClaimFailureKind::Unreachable) => {
                Some((t(locale, Key::AccountUnreachableError), palette.destructive))
            }
            AccountClaimState::Done { already: true } => Some((t(locale, Key::AccountAlreadyClaimedInfo), palette.success)),
            AccountClaimState::Idle | AccountClaimState::InFlight | AccountClaimState::Done { already: false } => None,
        }
    };
    body = body.child(status_line(status, palette));

    let button_label = if created {
        t(locale, Key::AccountCreatedButton)
    } else if in_flight {
        t(locale, Key::AccountCreatingButton)
    } else {
        t(locale, Key::AccountCreateButton)
    };

    body = body.child(widgets::step_button("oobe-create-account", button_label, StepButtonVariant::Primary, created || in_flight, palette, create_click));

    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(20.))
        .child(widgets::title(t(locale, Key::AccountTitle), palette))
        .child(widgets::subtitle(t(locale, Key::AccountSubtitle), palette))
        .child(widgets::card(body, palette))
}

/// The "建立帳號" submit — validates both fields, then dispatches the
/// gateway claim on a background thread. Extracted out of `render`'s
/// `create_click` closure (WP-oobe-enter, 2026-08-23) so it has exactly ONE
/// body reachable from TWO triggers: the button's own click, and
/// `main.rs`'s `on_oobe_next` (bound to Enter) via `super::handle_enter_
/// submit` — see `OobeFlow::enter_outcome`'s own doc comment in `state.rs`
/// for why Enter needs this at all: without it, Enter on this step was a
/// silent no-op for as long as the account hadn't been created yet, since
/// `next_with_wired` alone can never satisfy `AccountCreate`'s own
/// precondition (`account_created` only flips on a server-confirmed
/// outcome). Reads `view.oobe_account_fields` fresh at call time — same
/// "re-borrow at invocation, not at render time" discipline `render.rs`'s
/// `button_row` closures already establish — rather than taking pre-cloned
/// `Entity<OobeTextField>` handles as parameters, so a keyboard-triggered
/// call needs nothing beyond `&mut ShellView`.
pub(super) fn try_submit(view: &mut ShellView, cx: &mut Context<ShellView>) {
    if view.oobe_ui.account_claim == AccountClaimState::InFlight {
        // A trigger landing mid-flight is a no-op. The button is ALSO
        // visually disabled while `InFlight` (see `render`'s own `disabled`
        // arg), and `OobeFlow::enter_outcome` refuses to route Enter here at
        // all while in flight — this guard is the authoritative one
        // regardless of which of the two callers reached it.
        return;
    }
    let name = view.oobe_account_fields.name.read(cx).content(cx).trim().to_string();
    let password = view.oobe_account_fields.password.read(cx).content(cx);
    if name.is_empty() || password.is_empty() {
        view.oobe_ui.set_account_validation_error(true);
        view.oobe_ui.reset_account_claim();
        cx.notify();
        return;
    }
    view.oobe_ui.set_account_validation_error(false);

    // Dev escape — see this file's header comment and `main.rs`'s env-var
    // list. Skips the network entirely and reproduces round 2's original
    // local-only click verbatim (no password-length gate either — matching
    // that behavior exactly, not the real gateway rule below).
    // Q1 (2026-08-24): behind the shipping gate — this skips the device-claim
    // RPC entirely, so a shipping build must never take it. See
    // `crate::shipping`.
    if crate::shipping::debug_env_is_one("DUDUCLAW_SHELL_OOBE_LOCAL_ACCOUNT") {
        if let Some(flow) = view.oobe.as_mut() {
            flow.set_operator_name(&name);
            flow.set_account_created(true);
            crate::oobe::save_state(flow.state());
        }
        view.oobe_ui.reset_account_claim();
        cx.notify();
        return;
    }

    if password.chars().count() < 8 {
        // Mirrors `handle_first_run_claim`'s own `< 8 chars` rule
        // (`duduclaw-gateway/src/server.rs`) so the operator learns this
        // without a round trip. Caught here BEFORE `set_operator_name`/
        // `save_state`/`InFlight` — nothing has changed yet, so this is a
        // pure no-network branch.
        view.oobe_ui.set_account_claim_failed(AccountClaimFailureKind::PasswordTooShort);
        cx.notify();
        return;
    }

    if let Some(flow) = view.oobe.as_mut() {
        flow.set_operator_name(&name);
        crate::oobe::save_state(flow.state());
    }
    view.oobe_ui.set_account_claim_in_flight();
    cx.notify();

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = claim::create_account(&password);
        // The receiver only goes away if `ShellView` itself was torn down
        // mid-flight (window closed) — nothing actionable there, same "best
        // effort, never panic" contract `oobe::save_state` already follows
        // for its own I/O failures.
        let _ = tx.send(result);
    });

    // One-shot poll: `try_recv` + a paced background-executor timer, same
    // mechanics as `duduclaw-native-gui/src/main.rs`'s own persistent
    // bridge loop (see this file's header comment) but this task exits
    // itself the moment a result arrives (or the sender is dropped) rather
    // than running for the window's whole lifetime.
    cx.spawn(async move |weak, cx| loop {
        match rx.try_recv() {
            Ok(result) => {
                let _ = weak.update(cx, |view, cx| {
                    apply_claim_result(view, result);
                    cx.notify();
                });
                break;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
        }
        cx.background_executor().timer(std::time::Duration::from_millis(50)).await;
    })
    .detach();
}

/// Applies a settled `claim::create_account` result to `ShellView` — the
/// weak-entity update body run from `cx.spawn`'s poll loop above. `created`
/// (the flow-advance authority `OobeFlow::can_advance` reads) only ever
/// flips `true` on `Claimed`/`AlreadyClaimed`, matching
/// `AccountClaimState::Done`'s own doc comment in `oobe/mod.rs`.
fn apply_claim_result(view: &mut ShellView, result: Result<claim::ClaimOutcome, claim::ClaimError>) {
    match result {
        Ok(claim::ClaimOutcome::Claimed) => {
            if let Some(flow) = view.oobe.as_mut() {
                flow.set_account_created(true);
                crate::oobe::save_state(flow.state());
            }
            view.oobe_ui.set_account_claim_done(false);
        }
        Ok(claim::ClaimOutcome::AlreadyClaimed) => {
            if let Some(flow) = view.oobe.as_mut() {
                flow.set_account_created(true);
                crate::oobe::save_state(flow.state());
            }
            view.oobe_ui.set_account_claim_done(true);
        }
        Err(claim::ClaimError::RejectedTooShort) => {
            // Reachable in practice only if the client-side pre-check above
            // and the gateway's own rule ever drift.
            view.oobe_ui.set_account_claim_failed(AccountClaimFailureKind::PasswordTooShort);
        }
        Err(other) => {
            // Diagnostic detail (which of Unreachable/Http/Malformed/
            // NonLoopback happened) goes to stderr only — the OOBE surface
            // collapses all of these to one retryable message, see
            // `AccountClaimFailureKind`'s own doc comment in `oobe/mod.rs`.
            eprintln!("[oobe/account] first-run claim failed: {other:?}");
            view.oobe_ui.set_account_claim_failed(AccountClaimFailureKind::Unreachable);
        }
    }
}

fn labeled_field(label: &'static str, field: gpui::Entity<widgets::OobeTextField>, palette: crate::palette::ShellPalette) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(label))
        .child(field)
}

/// Shared small-text line for both the (red) validation/claim-failure
/// messages and the (green) already-claimed info line — `color` is the
/// already-resolved `ShellPalette` token (`.destructive` or `.success`) the
/// caller picked; this helper is just the shared size/layout.
fn message_line(text: &'static str, color: u32) -> Div {
    message_line_with_alpha(text, color, 1.0)
}

/// `message_line`'s own implementation, generalized over the alpha factor
/// so `status_line`'s invisible placeholder (alpha 0) can reuse the EXACT
/// same size/shape rather than hand-duplicating it — the whole point of the
/// placeholder is to be pixel-identical in layout to a real message, just
/// unseen.
fn message_line_with_alpha(text: &'static str, color: u32, alpha_factor: f32) -> Div {
    div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(color, alpha_factor)).child(text)
}

/// The `AccountCreate` step's status slot — see `render`'s own call site
/// comment (W7-2, OOBE-acct-stuck) for why this is unconditionally present
/// rather than appended-or-not. `status` is `None` on the two states that
/// have nothing to say (`Idle`/`InFlight`/freshly-claimed `Done{already:
/// false}` — that last one flips `created` instead, which switches the
/// button label, not this slot); `Some((text, color))` otherwise. Reuses
/// `message_line_with_alpha` at alpha 0 with a non-breaking space (not an
/// empty string — an empty text child can collapse a line's height to
/// zero, defeating the whole point of reserving space) so the placeholder
/// occupies the SAME height a real one-line message would.
fn status_line(status: Option<(&'static str, u32)>, palette: crate::palette::ShellPalette) -> Div {
    match status {
        Some((text, color)) => message_line(text, color),
        None => message_line_with_alpha("\u{a0}", palette.destructive, 0.0),
    }
}

#[cfg(test)]
mod tests {
    // ── W7-2 (OOBE-acct-stuck, 2026-08-24) ──────────────────────────────
    // Root cause, live-reproduced on a scratch VM clone
    // (`appliance/.vm/w72-evidence/`): `render`'s status message used to be
    // appended to `body` only when there was something to show, which
    // shifted the "建立帳號" button down by the message line's height the
    // instant a validation/claim error appeared. A resubmit click aimed at
    // the button's PRE-error screen position could then land on the (now
    // inert) message text instead of the button — `message_line` has no
    // click handler at all — leaving the screen showing the exact same
    // "請輸入操作者名稱與密碼" text no matter how many times the operator
    // retried, indistinguishable from `try_submit` itself refusing to run
    // again. `try_submit`'s own state transitions were verified correct via
    // the same VM repro (empty→error→filled→a DIFFERENT, correct message
    // each time when clicked at the button's CURRENT position) — the bug is
    // purely this layout instability, not `account_validation_error`/
    // `AccountClaimState` bookkeeping.
    //
    // Same "crude but load-bearing" source-scan shape this crate already
    // uses for gpui closures a plain unit test cannot drive (no
    // `TestAppContext` window round-trip for one assertion; see
    // `oobe/render.rs`'s and `main.rs`'s own test modules for precedent).

    #[test]
    fn the_status_slot_is_appended_unconditionally_so_the_button_never_moves() {
        let source = include_str!("account.rs");
        assert!(
            source.contains("\n    body = body.child(status_line(status, palette));\n"),
            "status_line must be appended as a top-level statement in `render` (4-space \
             indent) — not nested inside an `if`/`match` arm — or the button's Y position goes \
             back to depending on whether a status message is showing (OOBE-acct-stuck)"
        );
    }

    #[test]
    fn status_line_has_a_placeholder_arm_for_the_no_message_case() {
        let source = include_str!("account.rs");
        let start = source.find("fn status_line(").expect("status_line not found in oobe/steps/account.rs");
        let window = &source[start..(start + 700).min(source.len())];
        assert!(
            window.contains("None =>"),
            "status_line must handle the no-message case with a real (placeholder) render, not \
             by omitting the child — an omitted child reintroduces the exact same layout-shift \
             bug one layer down"
        );
        assert!(
            window.contains("message_line_with_alpha"),
            "the placeholder arm must reuse message_line_with_alpha so its reserved height \
             matches a real one-line message's height exactly"
        );
    }
}
