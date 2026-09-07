//! OOBE step: AI runtime authorization.
//!
//! 2026-09-05 (QEMU feature walkthrough): "立即設定" used to flip a local
//! flag (`OobeFlow::set_runtime_authorized`) and make NO gateway call, so a
//! machine that had "authorized" its runtime still had no credential and
//! every delegated goal task died on dispatch. The step was rewritten to
//! store ONE Anthropic API key through the dashboard's own `accounts.add`
//! RPC.
//!
//! ── WP-C (2026-09-05) — one key became a provider list ───────────────────
//! `docs/todo/TODO-ai-runtimes-2026-09.md` §3 WP-C. Decision 2 of that TODO
//! ships every runtime in the image, so the one screen where an operator is
//! asked to hand the machine a credential now lists all of them
//! (`oobe::runtime_providers::PROVIDERS`), each row with two actions:
//!
//!   「輸入 API 金鑰」 expands the step's single masked field for that row and
//!   stores what is typed through `accounts.add`, now carrying `provider`
//!   (`gateway_client::add_api_key_account`) under the account id
//!   `oobe-<provider>`.
//!
//!   「登入帳號」 (only for providers whose CLI has an interactive login)
//!   shows decision 1B's risk disclosure FIRST — Anthropic and Google block
//!   consumer-subscription tokens in third-party products server-side and
//!   have suspended accounts over it; OpenAI has said nothing definite — and
//!   will not start anything until 「我了解風險，由我自行承擔」 is ticked
//!   (`OobeUiState::start_runtime_login` is the single gate; see
//!   `RuntimeLoginState::start_enabled`). It then drives the gateway's
//!   `auth.cli_login.*` flow, surfaces the device code / verification URL
//!   parsed out of the CLI's own transcript, and offers to open that URL in
//!   the appliance's browser.
//!
//! UNVERIFIED: needs live gateway. The `auth.cli_login.*` half is written
//! against the shapes on `main` today (see `gateway_client::cli_login`'s own
//! header comment, which carries the same marker and the full wire
//! contract); nothing in this file has been exercised against a running
//! gateway. The API-key half calls an RPC this step already used before this
//! round, with one added — and, until WP-A, ignored — `provider` field.
//!
//! Deferring is still one click, honestly labelled: without a credential,
//! 交辦 creates tasks that cannot run.

use std::sync::mpsc;
use std::time::Duration;

use gpui::{div, prelude::*, px, Context, Div, FontWeight, SharedString};

use duduclaw_native_gui::theme;

use crate::gateway_client::cli_login::{self, CliLoginFailure, CliLoginStatus, CliLoginUpdate};
use crate::gateway_client::{self, GatewayError};
use crate::i18n::{t, t1, Key, Locale};
use crate::oobe::runtime_providers::{self, ProviderEntry};
use crate::oobe::selections::RuntimeCredentialKind;
use crate::oobe::ui_state::{RuntimeClaimFailureKind, RuntimeClaimState, RuntimeLoginFailureKind, RuntimeLoginState};
use crate::oobe::widgets::{self, RuntimeAuthFields, StepButtonVariant};
use crate::oobe::{portal_browser, OobeFlow, OobeUiState};
use crate::palette::ShellPalette;
use crate::ShellView;

/// Shortest string we will even send: every provider key is far longer;
/// this only stops a stray keystroke from becoming a stored "credential".
const MIN_KEY_LEN: usize = 20;

/// `overlay/notifications.rs::POLL_INTERVAL`'s exact value.
const BRIDGE_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Height cap on the provider list. Seventeen rows do not fit a 900px screen
/// alongside the title, the hint and the bottom nav, so this is the first
/// scroll container in this crate (`settings/mod.rs`'s own header comment
/// records that there were none before) — the alternative, an explicit "還有
/// N 家" truncation like the settings pages use for Wi-Fi lists, would hide
/// providers behind a count on the ONE screen whose job is to show them all.
const LIST_MAX_HEIGHT: f32 = 300.;

pub(super) fn render(flow: &OobeFlow, ui: &OobeUiState, fields: &RuntimeAuthFields, cx: &mut Context<ShellView>) -> Div {
    let locale = flow.locale();
    let palette = flow.palette();

    let defer_click = cx.listener(|view, _ev, _window, cx| {
        cancel_any_live_login(view);
        if let Some(flow) = view.oobe.as_mut() {
            flow.skip();
            crate::oobe::save_state(flow.state());
        }
        cx.notify();
    });

    let mut list = div()
        .id("oobe-runtime-providers")
        .flex()
        .flex_col()
        .gap(px(2.))
        .max_h(px(LIST_MAX_HEIGHT))
        .overflow_y_scroll();
    for (index, entry) in runtime_providers::PROVIDERS.iter().enumerate() {
        list = list.child(provider_row(*entry, index, flow, ui, fields, cx));
    }

    let body = div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(t(locale, Key::RuntimeAuthKeyHint)))
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.text_faint, 1.0)).child(t(locale, Key::RuntimeAuthProvidersHint)))
        .child(list)
        .child(widgets::step_button("oobe-runtime-defer", t(locale, Key::RuntimeAuthDeferLater), StepButtonVariant::Ghost, false, palette, defer_click));

    let mut column = div().flex().flex_col().items_center().gap(px(20.));
    if let Some(icon) = crate::icons::icon_or_none(&[(crate::icons::KEY, palette.muted_foreground)], 32.) {
        column = column.child(icon);
    }
    column
        .child(widgets::title(t(locale, Key::RuntimeAuthTitle), palette))
        .child(widgets::subtitle(t(locale, Key::RuntimeAuthSubtitle), palette))
        .child(widgets::card(body, palette))
}

/// One provider. The header line is always there; at most one of the two
/// panels below it is, and only on the row that owns it.
fn provider_row(
    entry: ProviderEntry,
    index: usize,
    flow: &OobeFlow,
    ui: &OobeUiState,
    fields: &RuntimeAuthFields,
    cx: &mut Context<ShellView>,
) -> Div {
    let locale = flow.locale();
    let palette = flow.palette();
    let kind = flow.runtime_provider_kind(entry.id);
    let key_open = ui.runtime_key_provider == Some(entry.id);
    let login_open = ui.runtime_login.provider() == Some(entry.id);

    let (status_text, status_color) = match kind {
        None => (t(locale, Key::RuntimeAuthStatusUnset), palette.muted_foreground),
        Some(RuntimeCredentialKind::ApiKey) => (t(locale, Key::RuntimeAuthStatusKeySaved), palette.success),
        Some(RuntimeCredentialKind::Login) => (t(locale, Key::RuntimeAuthStatusLoggedIn), palette.success),
    };

    // A row whose CLI login this build cannot start says so in place of the
    // generic note — see `runtime_providers`'s header comment for why the
    // button must stay inert rather than authenticate the wrong vendor.
    let login_blocked = entry.login_runtime.is_some() && !entry.login_dispatchable();
    let note = if login_blocked { t(locale, Key::RuntimeAuthLoginUnavailableHere) } else { t(locale, entry.note_key()) };

    let key_click = cx.listener(move |view, _ev, _window, cx| {
        // A key typed for one provider must never be carried into another
        // row's field — the operator would silently store the wrong secret
        // under the wrong provider.
        view.oobe_runtime_fields.api_key.update(cx, |field, cx| field.clear(cx));
        view.oobe_ui.toggle_runtime_key_field(entry.id);
        cx.notify();
    });
    let login_click = cx.listener(move |view, _ev, _window, cx| {
        view.oobe_ui.open_runtime_login_disclosure(entry.id);
        cx.notify();
    });

    let mut actions = div().flex().items_center().gap(px(8.)).child(widgets::small_button(
        ("oobe-runtime-key", index),
        t(locale, Key::RuntimeAuthEnterKeyAction),
        if key_open { StepButtonVariant::Secondary } else { StepButtonVariant::Ghost },
        false,
        palette,
        key_click,
    ));
    if entry.login_runtime.is_some() {
        actions = actions.child(widgets::small_button(
            ("oobe-runtime-login", index),
            t(locale, Key::RuntimeAuthLoginAction),
            if login_open { StepButtonVariant::Secondary } else { StepButtonVariant::Ghost },
            // Inert for a runtime the gateway cannot name, and while another
            // provider's login already owns the machine's one PTY.
            login_blocked || (ui.runtime_login.is_busy() && !login_open),
            palette,
            login_click,
        ));
    }

    let header = div()
        .flex()
        .items_center()
        .gap(px(10.))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().text_size(px(theme::TEXT_SM)).font_weight(FontWeight::MEDIUM).text_color(theme::alpha(palette.foreground, 1.0)).child(entry.name))
                .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(note)),
        )
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(status_color, 1.0)).child(status_text))
        .child(actions);

    let mut row = div().flex().flex_col().gap(px(10.)).py(px(8.)).child(header);
    if key_open {
        row = row.child(key_panel(entry, index, ui, fields, locale, palette, cx));
    }
    if login_open {
        row = row.child(login_panel(index, &ui.runtime_login, locale, palette, cx));
    }
    row
}

/// The expanded masked-key form for one row.
fn key_panel(
    entry: ProviderEntry,
    index: usize,
    ui: &OobeUiState,
    fields: &RuntimeAuthFields,
    locale: Locale,
    palette: ShellPalette,
    cx: &mut Context<ShellView>,
) -> Div {
    let in_flight = ui.runtime_claim == RuntimeClaimState::InFlight;
    let status: Option<(&str, u32)> = match ui.runtime_claim {
        RuntimeClaimState::Idle => None,
        RuntimeClaimState::InFlight => Some((t(locale, Key::RuntimeAuthSaving), palette.muted_foreground)),
        RuntimeClaimState::Saved => Some((t(locale, Key::RuntimeAuthAuthorized), palette.success)),
        RuntimeClaimState::Failed(kind) => Some((
            match kind {
                RuntimeClaimFailureKind::Empty => t(locale, Key::RuntimeAuthKeyEmpty),
                RuntimeClaimFailureKind::LooksWrong => t(locale, Key::RuntimeAuthKeyLooksWrong),
                RuntimeClaimFailureKind::Unreachable => t(locale, Key::RuntimeAuthUnreachable),
            },
            palette.destructive,
        )),
    };

    let save_click = cx.listener(|view, _ev, _window, cx| {
        try_submit(view, cx);
    });
    let collapse_click = cx.listener(|view, _ev, _window, cx| {
        view.oobe_runtime_fields.api_key.update(cx, |field, cx| field.clear(cx));
        view.oobe_ui.close_runtime_key_field();
        cx.notify();
    });

    let mut panel = div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .p(px(12.))
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(palette.app_shell, 1.0))
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::alpha(palette.foreground, 1.0))
                .child(SharedString::from(t1(locale, Key::RuntimeAuthKeyFor, entry.name))),
        )
        .child(fields.api_key.clone());
    if let Some((text, color)) = status {
        panel = panel.child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(color, 1.0)).child(text));
    }
    panel.child(
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(widgets::small_button(
                ("oobe-runtime-save-key", index),
                if in_flight { t(locale, Key::RuntimeAuthSaving) } else { t(locale, Key::RuntimeAuthSaveKey) },
                StepButtonVariant::Primary,
                in_flight,
                palette,
                save_click,
            ))
            .child(widgets::small_button(("oobe-runtime-collapse-key", index), t(locale, Key::RuntimeAuthKeyCollapse), StepButtonVariant::Ghost, false, palette, collapse_click)),
    )
}

/// The 「登入帳號」 panel: disclosure → running → settled.
fn login_panel(index: usize, state: &RuntimeLoginState, locale: Locale, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let close_click = cx.listener(|view, _ev, _window, cx| {
        view.oobe_ui.close_runtime_login();
        cx.notify();
    });

    let panel = div().flex().flex_col().gap(px(8.)).p(px(12.)).rounded(px(theme::RADIUS_LG)).bg(theme::alpha(palette.app_shell, 1.0));

    match state {
        // Unreachable in practice (this fn is only called for the row that
        // owns the state), kept exhaustive rather than `unreachable!()`.
        RuntimeLoginState::Idle => panel,

        RuntimeLoginState::Disclosure { acknowledged, .. } => {
            let acknowledged = *acknowledged;
            let ack_click = cx.listener(|view, _ev, _window, cx| {
                view.oobe_ui.toggle_runtime_login_acknowledged();
                cx.notify();
            });
            let start_click = cx.listener(|view, _ev, _window, cx| {
                start_login(view, cx);
            });
            panel
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::alpha(palette.destructive, 1.0))
                        .child(t(locale, Key::RuntimeAuthRiskTitle)),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .line_height(px(theme::TEXT_XS * 1.6))
                        .text_color(theme::alpha(palette.muted_foreground, 1.0))
                        .child(t(locale, Key::RuntimeAuthRiskBody)),
                )
                .child(
                    div()
                        .id(("oobe-runtime-login-ack", index))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .cursor_pointer()
                        .child(widgets::check_box(acknowledged, palette))
                        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.foreground, 1.0)).child(t(locale, Key::RuntimeAuthRiskAck)))
                        .on_click(ack_click),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(widgets::small_button(
                            ("oobe-runtime-login-start", index),
                            t(locale, Key::RuntimeAuthLoginStart),
                            StepButtonVariant::Primary,
                            // The §1-1 gate, rendered: no tick, no button.
                            !acknowledged,
                            palette,
                            start_click,
                        ))
                        .child(widgets::small_button(("oobe-runtime-login-close", index), t(locale, Key::RuntimeAuthLoginClose), StepButtonVariant::Ghost, false, palette, close_click)),
                )
        }

        RuntimeLoginState::Starting { .. } => {
            panel.child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(t(locale, Key::RuntimeAuthLoginStarting)))
        }

        RuntimeLoginState::Running { url, code, .. } => {
            let cancel_click = cx.listener(|view, _ev, _window, cx| {
                cancel_any_live_login(view);
                cx.notify();
            });
            let mut running = panel
                .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(t(locale, Key::RuntimeAuthLoginWaiting)));
            if let Some(code) = code.clone() {
                running = running.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(t(locale, Key::RuntimeAuthLoginCodeLabel)))
                        // The code is what the operator types into the
                        // verification page, so it is the largest thing on
                        // this panel.
                        .child(
                            div()
                                .text_size(px(theme::TEXT_2XL))
                                .font_weight(FontWeight::BOLD)
                                .text_color(theme::alpha(palette.foreground, 1.0))
                                .child(SharedString::from(code)),
                        ),
                );
            }
            if let Some(url) = url.clone() {
                let for_click = url.clone();
                let open_click = cx.listener(move |_view, _ev, _window, _cx| {
                    // Validated inside `open_login_page` — the URL was
                    // parsed out of terminal output and never reaches a
                    // browser's argv unchecked.
                    portal_browser::open_login_page(&for_click);
                });
                running = running.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(t(locale, Key::RuntimeAuthLoginUrlLabel)))
                        .child(
                            div()
                                .text_size(px(theme::TEXT_XS))
                                .text_color(theme::alpha(palette.foreground, 1.0))
                                .child(SharedString::from(url)),
                        )
                        .child(widgets::small_button(
                            ("oobe-runtime-login-open", index),
                            t(locale, Key::RuntimeAuthLoginOpenBrowser),
                            StepButtonVariant::Primary,
                            false,
                            palette,
                            open_click,
                        )),
                );
            }
            running.child(widgets::small_button(("oobe-runtime-login-cancel", index), t(locale, Key::RuntimeAuthLoginCancel), StepButtonVariant::Ghost, false, palette, cancel_click))
        }

        RuntimeLoginState::Succeeded { .. } => panel
            .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.success, 1.0)).child(t(locale, Key::RuntimeAuthLoginSucceeded)))
            .child(widgets::small_button(("oobe-runtime-login-close", index), t(locale, Key::RuntimeAuthLoginClose), StepButtonVariant::Ghost, false, palette, close_click)),

        RuntimeLoginState::Failed { kind, .. } => panel
            .child(
                div()
                    .text_size(px(theme::TEXT_XS))
                    .text_color(theme::alpha(palette.destructive, 1.0))
                    .child(t(
                        locale,
                        match kind {
                            RuntimeLoginFailureKind::Unavailable => Key::RuntimeAuthLoginErrUnavailable,
                            RuntimeLoginFailureKind::Unreachable => Key::RuntimeAuthLoginErrUnreachable,
                            RuntimeLoginFailureKind::Refused => Key::RuntimeAuthLoginErrRefused,
                            RuntimeLoginFailureKind::Abandoned => Key::RuntimeAuthLoginErrAbandoned,
                        },
                    )),
            )
            .child(widgets::small_button(("oobe-runtime-login-close", index), t(locale, Key::RuntimeAuthLoginClose), StepButtonVariant::Ghost, false, palette, close_click)),
    }
}

/// Validate locally, then store the key through `accounts.add` on a worker
/// thread — the same thread + mpsc + `cx.spawn` bridge `steps::account`
/// uses for its claim. Guarded against double clicks while in flight.
pub(super) fn try_submit(view: &mut ShellView, cx: &mut Context<ShellView>) {
    if view.oobe_ui.runtime_claim == RuntimeClaimState::InFlight {
        return;
    }
    // Which row's field is on screen IS which provider this key belongs to.
    // No expanded row means no target — never a silent default provider.
    let Some(provider) = view.oobe_ui.runtime_key_provider else {
        return;
    };
    let key = view.oobe_runtime_fields.api_key.read(cx).content(cx).trim().to_string();
    if key.is_empty() {
        view.oobe_ui.set_runtime_claim(RuntimeClaimState::Failed(RuntimeClaimFailureKind::Empty));
        cx.notify();
        return;
    }
    if !looks_like_api_key(&key) {
        view.oobe_ui.set_runtime_claim(RuntimeClaimState::Failed(RuntimeClaimFailureKind::LooksWrong));
        cx.notify();
        return;
    }
    view.oobe_ui.set_runtime_claim(RuntimeClaimState::InFlight);
    cx.notify();

    let existing_jwt = view.overlay_ui.notifications.session_jwt().map(str::to_string);
    let account_id = runtime_providers::account_id(provider);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let run = || -> Result<(), GatewayError> {
            let jwt = match existing_jwt {
                Some(jwt) => jwt,
                None => gateway_client::bootstrap_local_session()?,
            };
            gateway_client::add_api_key_account(&jwt, provider, &account_id, &key)?;
            Ok(())
        };
        let _ = tx.send(run());
    });
    cx.spawn(async move |weak, cx| loop {
        match rx.try_recv() {
            Ok(result) => {
                let _ = weak.update(cx, |view, cx| {
                    apply_result(view, provider, result, cx);
                    cx.notify();
                });
                break;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => break,
        }
        cx.background_executor().timer(BRIDGE_POLL_INTERVAL).await;
    })
    .detach();
}

fn apply_result(view: &mut ShellView, provider: &'static str, result: Result<(), GatewayError>, cx: &mut Context<ShellView>) {
    match result {
        Ok(()) => {
            view.oobe_ui.set_runtime_claim(RuntimeClaimState::Saved);
            // The plaintext key has served its purpose; do not leave it in
            // the widget for the next row to inherit.
            view.oobe_runtime_fields.api_key.update(cx, |field, cx| field.clear(cx));
            if let Some(flow) = view.oobe.as_mut() {
                flow.record_runtime_provider(provider, RuntimeCredentialKind::ApiKey);
                crate::oobe::save_state(flow.state());
            }
        }
        Err(e) => {
            // Always logged: once per click, and the only trace besides the
            // status line.
            eprintln!("[oobe/runtime_auth] accounts.add failed for provider {provider}: {e:?}");
            view.oobe_ui.set_runtime_claim(RuntimeClaimState::Failed(RuntimeClaimFailureKind::Unreachable));
        }
    }
}

/// Starts the CLI login for whichever provider's disclosure is on screen —
/// and ONLY if that disclosure has been acknowledged, which
/// `OobeUiState::start_runtime_login` is the single authority on.
fn start_login(view: &mut ShellView, cx: &mut Context<ShellView>) {
    let Some(provider) = view.oobe_ui.start_runtime_login() else {
        return;
    };
    // Fail closed: a runtime the gateway cannot name would be resolved to
    // Claude and would authenticate the wrong vendor. The button is already
    // inert for these rows; this is the second gate, at the point of action.
    let runtime = runtime_providers::find(provider).filter(|e| e.login_dispatchable()).and_then(|e| e.login_runtime);
    let Some(runtime) = runtime else {
        view.oobe_ui.set_runtime_login_failed(RuntimeLoginFailureKind::Unavailable);
        cx.notify();
        return;
    };
    cx.notify();

    let existing_jwt = view.overlay_ui.notifications.session_jwt().map(str::to_string);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let jwt = match existing_jwt {
            Some(jwt) => jwt,
            None => match gateway_client::bootstrap_local_session() {
                Ok(jwt) => jwt,
                Err(e) => {
                    let _ = tx.send(CliLoginUpdate::Failed { kind: CliLoginFailure::Unreachable, detail: format!("{e:?}") });
                    return;
                }
            },
        };
        cli_login::run_login_session(jwt, runtime, tx);
    });

    cx.spawn(async move |weak, cx| loop {
        match rx.try_recv() {
            Ok(update) => {
                let mut done = false;
                let updated = weak.update(cx, |view, cx| {
                    done = apply_login_update(view, provider, update);
                    cx.notify();
                });
                // The view is gone (window closed) — stop polling.
                if updated.is_err() || done {
                    break;
                }
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => break,
        }
        cx.background_executor().timer(BRIDGE_POLL_INTERVAL).await;
    })
    .detach();
}

/// Applies one worker update. Returns `true` when the login has settled and
/// the poll loop should stop.
fn apply_login_update(view: &mut ShellView, provider: &'static str, update: CliLoginUpdate) -> bool {
    match update {
        CliLoginUpdate::Started { session_id } => {
            view.oobe_ui.set_runtime_login_session(session_id);
            false
        }
        CliLoginUpdate::Prompt { url, code } => {
            view.oobe_ui.set_runtime_login_prompt(url, code);
            false
        }
        CliLoginUpdate::Settled { status, registered } => {
            match status {
                CliLoginStatus::Succeeded => {
                    // Recorded even if the operator has since closed the
                    // panel: the credential really is on the machine now,
                    // and the row's badge must say so.
                    if let Some(flow) = view.oobe.as_mut() {
                        flow.record_runtime_provider(provider, RuntimeCredentialKind::Login);
                        crate::oobe::save_state(flow.state());
                    }
                    if !registered {
                        // Expected for every CLI that keeps its own
                        // credential store — see `cli_login::finalize`.
                        eprintln!("[oobe/runtime_auth] {provider} signed in; no token to register (the CLI keeps its own credential store)");
                    }
                    view.oobe_ui.set_runtime_login_succeeded();
                }
                CliLoginStatus::Failed => view.oobe_ui.set_runtime_login_failed(RuntimeLoginFailureKind::Refused),
                CliLoginStatus::Exited => view.oobe_ui.set_runtime_login_failed(RuntimeLoginFailureKind::Abandoned),
            }
            true
        }
        CliLoginUpdate::Failed { kind, detail } => {
            eprintln!("[oobe/runtime_auth] {provider} login failed: {detail}");
            view.oobe_ui.set_runtime_login_failed(match kind {
                CliLoginFailure::Rejected => RuntimeLoginFailureKind::Unavailable,
                CliLoginFailure::Unreachable => RuntimeLoginFailureKind::Unreachable,
            });
            true
        }
    }
}

/// Kills a live PTY login on the gateway, then forgets it. Called from the
/// panel's own 取消登入 button and from 稍後再說 — leaving a login process
/// running on the appliance after the operator has moved on would be a
/// resource leak the operator has no way to see, let alone stop.
///
/// The bottom-nav 繼續/返回/略過 buttons (`oobe::render::button_row`) do NOT
/// route through here — they are generic across all ten steps. A login left
/// behind that way is bounded by the worker's own `OVERALL_BUDGET`, which
/// cancels the session itself before giving up (see
/// `gateway_client::cli_login::run_login_session`).
fn cancel_any_live_login(view: &mut ShellView) {
    if let Some(session_id) = view.oobe_ui.runtime_login.session_id().map(str::to_string) {
        let existing_jwt = view.overlay_ui.notifications.session_jwt().map(str::to_string);
        std::thread::spawn(move || {
            let jwt = match existing_jwt {
                Some(jwt) => jwt,
                None => match gateway_client::bootstrap_local_session() {
                    Ok(jwt) => jwt,
                    Err(e) => {
                        eprintln!("[oobe/runtime_auth] could not reach the gateway to cancel login session {session_id}: {e:?}");
                        return;
                    }
                },
            };
            if let Err(e) = cli_login::cancel(&jwt, &session_id) {
                eprintln!("[oobe/runtime_auth] cancelling login session {session_id} failed: {e:?}");
            }
        });
    }
    view.oobe_ui.close_runtime_login();
}

/// Cheap shape check, deliberately loose: long enough and no whitespace.
/// Never encodes a provider's exact prefix — those change, and this screen
/// now accepts keys from seventeen different providers.
pub(crate) fn looks_like_api_key(key: &str) -> bool {
    key.len() >= MIN_KEY_LEN && !key.chars().any(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_shape_check_rejects_short_or_spaced_input_only() {
        assert!(!looks_like_api_key(""));
        assert!(!looks_like_api_key("sk-ant"));
        assert!(!looks_like_api_key("sk-ant-api03 with a space in it here"));
        assert!(looks_like_api_key("sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789"));
        assert!(looks_like_api_key("some-other-provider-key-format-1234567890"));
    }

    /// Source-scan guard, same crude-but-load-bearing shape `oobe/render.rs`'s
    /// own test module uses for gpui closures a plain unit test cannot drive.
    /// The §1-1 gate is a two-part rule — a DISABLED start button and a
    /// refusal inside the click path — and losing either half silently turns
    /// the disclosure into decoration.
    #[test]
    fn the_login_start_path_is_gated_on_the_acknowledgement_twice() {
        let source = include_str!("runtime_auth.rs");
        let start = source.find("\"oobe-runtime-login-start\", index").expect("the start button must exist");
        let window = &source[start..(start + 400).min(source.len())];
        assert!(
            window.contains("!acknowledged"),
            "the 開始登入 button must be disabled until the risk disclosure is acknowledged — \
             `widgets::small_button` attaches no click handler at all while disabled"
        );
        let click = source.find("fn start_login(").expect("start_login must exist");
        let body = &source[click..(click + 700).min(source.len())];
        assert!(
            body.contains("view.oobe_ui.start_runtime_login()"),
            "start_login must go through OobeUiState::start_runtime_login, the single acknowledgement gate"
        );
        assert!(
            body.contains("login_dispatchable()"),
            "start_login must refuse a runtime the gateway cannot name — it would resolve to Claude and authenticate the wrong vendor"
        );
    }
}
