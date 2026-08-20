// Screen 2 — login. Re-skinned to MDS (Multica-derived Design System, see
// `theme.rs` + `commercial/docs/multica-redesign-spec.md`): a Card (spec
// §4) holding two labeled Input fields + a `brand`-variant submit Button +
// a `link`-variant secondary action, sitting on the app-shell backdrop.
//
// S2: the submit button is wired to real auth (`api.rs` via the
// `ws_status.rs` session manager) — `POST /api/login` on click, loading/
// disabled state while the request is in flight, a localized error line on
// failure. Success is handled entirely by `RootView::handle_session_event`
// (`main.rs`) reacting to the background manager's `LoginOk` event, not
// here — this file only ever fires the request and renders whatever state
// `RootView` currently holds.

use gpui::{div, prelude::*, px, Context, Div};

use crate::i18n;
use crate::theme;
use crate::ws_status::Command as SessionCommand;
use crate::RootView;

/// Field label — MDS "meta/label" type bucket (spec §2 type table: `text-xs
/// text-muted-foreground`), not the body-text size the Calm Glass version
/// used.
fn field_label(text: impl Into<gpui::SharedString>) -> Div {
    div()
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(text.into())
}

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Div {
    let locale = state.locale;

    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme::alpha(theme::APP_SHELL, 1.0))
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_6()
                .w(px(380.))
                // ── Wordmark + subtitle ──────────────────────────────
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_1()
                        .child(div().text_size(px(36.)).child("🐾"))
                        .child(
                            // "卡片/dialog 標題" scale: text-base font-medium.
                            div()
                                .text_size(px(theme::TEXT_BASE))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                                .child(i18n::t(locale, "app.name")),
                        )
                        .child(
                            div()
                                .text_size(px(theme::TEXT_XS))
                                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                                .child(i18n::t(locale, "login.subtitle")),
                        ),
                )
                // ── Card (spec §4: rounded-xl border-surface-border
                // bg-surface shadow-[var(--surface-shadow)], opaque — not
                // the Calm Glass translucent-panel look) ────────────────
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .p_4()
                        .rounded(px(theme::RADIUS_XL))
                        .bg(theme::alpha(theme::SURFACE, 1.0))
                        .border_1()
                        .border_color(theme::surface_border())
                        .shadow(theme::surface_shadow())
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1p5()
                                .child(field_label(i18n::t(locale, "login.email")))
                                .child(state.email_field.clone()),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1p5()
                                .child(field_label(i18n::t(locale, "login.password")))
                                .child(state.password_field.clone()),
                        )
                        // Inline error line (task item 3) — only rendered
                        // when the last attempt failed. Sits right above
                        // the submit button so it's the first thing seen
                        // after a failed click without shifting the fields
                        // above it.
                        .children(state.login_error.clone().map(|msg| {
                            div()
                                .text_size(px(theme::TEXT_XS))
                                .text_color(theme::alpha(theme::DESTRUCTIVE, 1.0))
                                .child(msg)
                        }))
                        // Submit — MDS Button `brand` variant, `lg` size
                        // (spec §4 Button: `h-9`, `bg-brand text-brand-
                        // foreground hover:bg-brand/90 active:bg-brand/85`).
                        // While `login_loading` is true the button shows a
                        // dimmed, non-interactive state (task item 3:
                        // "進 loading/disabled 態") — no hover/active/click
                        // handler attached at all, not just a visual dim.
                        .child({
                            let loading = state.login_loading;
                            div()
                                .id("login-submit")
                                .w_full()
                                .h_9()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(theme::RADIUS_LG))
                                .bg(theme::alpha(theme::BRAND, if loading { 0.5 } else { 1.0 }))
                                .text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0))
                                .text_size(px(theme::TEXT_SM))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(i18n::t(
                                    locale,
                                    if loading { "login.submitting" } else { "login.submit" },
                                ))
                                .when(!loading, |el| {
                                    el.cursor_pointer()
                                        .hover(|style| style.bg(theme::alpha(theme::BRAND, 0.90)))
                                        .active(|style| style.bg(theme::alpha(theme::BRAND, 0.85)))
                                        .on_click(cx.listener(|this, _ev, _window, cx| {
                                            if this.login_loading {
                                                return;
                                            }
                                            let email =
                                                this.email_field.read(cx).content.trim().to_string();
                                            let password =
                                                this.password_field.read(cx).content.clone();
                                            if email.is_empty() || password.is_empty() {
                                                // Client-side guard: don't fire a
                                                // request the server would just
                                                // reject as malformed. Reuses the
                                                // invalid-credentials message
                                                // rather than adding a dedicated
                                                // "required field" i18n key S2
                                                // doesn't otherwise need.
                                                let (key, _) = crate::api::AuthError::InvalidCredentials
                                                    .i18n_key_and_code();
                                                this.login_error = Some(i18n::t(this.locale, key));
                                                cx.notify();
                                                return;
                                            }
                                            this.login_loading = true;
                                            this.login_error = None;
                                            let _ = this.session_tx.send(SessionCommand::Login {
                                                email,
                                                password,
                                            });
                                            cx.notify();
                                        }))
                                })
                        })
                        // Secondary action — MDS Button `link` variant
                        // (spec §4: `text-primary underline-offset-4
                        // hover:underline` — relies on the underline, not
                        // color, for affordance; `--primary` in dark is a
                        // soft near-white, deliberately understated).
                        .child(
                            div()
                                .id("login-use-channel-code")
                                .w_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(theme::TEXT_SM))
                                .text_color(theme::alpha(theme::PRIMARY, 1.0))
                                .cursor_pointer()
                                .hover(|style| style.underline())
                                .child(i18n::t(locale, "login.useChannelCode")),
                        ),
                )
                // ── First-run hint ───────────────────────────────────
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(i18n::t(locale, "login.footer")),
                ),
        )
}
