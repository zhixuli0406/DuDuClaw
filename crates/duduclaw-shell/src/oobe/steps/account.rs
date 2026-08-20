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

    let name_entity = fields.name.clone();
    let password_entity = fields.password.clone();
    let create_click = cx.listener(move |view, _ev, _window, cx| {
        if view.oobe_ui.account_claim == AccountClaimState::InFlight {
            // A click landing mid-flight is a no-op. The button is ALSO
            // visually disabled while `InFlight` (see `disabled` below), so
            // in practice this only matters for the brief window between a
            // click event firing and gpui re-painting the disabled state —
            // this guard is the authoritative one either way.
            return;
        }
        let name = name_entity.read(cx).content.trim().to_string();
        let password = password_entity.read(cx).content.clone();
        if name.is_empty() || password.is_empty() {
            view.oobe_ui.set_account_validation_error(true);
            view.oobe_ui.reset_account_claim();
            cx.notify();
            return;
        }
        view.oobe_ui.set_account_validation_error(false);

        // Dev escape — see this file's header comment and `main.rs`'s
        // env-var list. Skips the network entirely and reproduces round 2's
        // original local-only click verbatim (no password-length gate
        // either — matching that behavior exactly, not the real gateway
        // rule below).
        if std::env::var("DUDUCLAW_SHELL_OOBE_LOCAL_ACCOUNT").is_ok_and(|v| v == "1") {
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
            // `save_state`/`InFlight` — nothing has changed yet, so this is
            // a pure no-network branch.
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
            // The receiver only goes away if `ShellView` itself was torn
            // down mid-flight (window closed) — nothing actionable there,
            // same "best effort, never panic" contract `oobe::save_state`
            // already follows for its own I/O failures.
            let _ = tx.send(result);
        });

        // One-shot poll: `try_recv` + a paced background-executor timer,
        // same mechanics as `duduclaw-native-gui/src/main.rs`'s own
        // persistent bridge loop (see this file's header comment) but this
        // task exits itself the moment a result arrives (or the sender is
        // dropped) rather than running for the window's whole lifetime.
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
    });

    let mut body = div()
        .flex()
        .flex_col()
        .gap(px(14.))
        .child(labeled_field(t(locale, Key::AccountNameLabel), fields.name.clone(), palette))
        .child(labeled_field(t(locale, Key::AccountPasswordLabel), fields.password.clone(), palette));

    if ui.account_validation_error {
        body = body.child(message_line(t(locale, Key::AccountValidationError), palette.destructive));
    } else {
        match ui.account_claim {
            AccountClaimState::Failed(AccountClaimFailureKind::PasswordTooShort) => {
                body = body.child(message_line(t(locale, Key::AccountPasswordTooShortError), palette.destructive));
            }
            AccountClaimState::Failed(AccountClaimFailureKind::Unreachable) => {
                body = body.child(message_line(t(locale, Key::AccountUnreachableError), palette.destructive));
            }
            AccountClaimState::Done { already: true } => {
                body = body.child(message_line(t(locale, Key::AccountAlreadyClaimedInfo), palette.success));
            }
            AccountClaimState::Idle | AccountClaimState::InFlight | AccountClaimState::Done { already: false } => {}
        }
    }

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

fn labeled_field(label: &'static str, field: gpui::Entity<widgets::OobeTextField>, palette: crate::oobe::palette::OobePalette) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(label))
        .child(field)
}

/// Shared small-text line for both the (red) validation/claim-failure
/// messages and the (green) already-claimed info line — `color` is the
/// already-resolved `OobePalette` token (`.destructive` or `.success`) the
/// caller picked; this helper is just the shared size/layout.
fn message_line(text: &'static str, color: u32) -> Div {
    div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(color, 1.0)).child(text)
}
