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

use gpui::{div, prelude::*, px, Context, Div};

use duduclaw_native_gui::theme;

use crate::i18n::{t, Key};
use crate::oobe::widgets::{AccountFields, StepButtonVariant};
use crate::oobe::{widgets, OobeFlow, OobeUiState};
use crate::ShellView;

pub(super) fn render(flow: &OobeFlow, ui: &OobeUiState, fields: &AccountFields, cx: &mut Context<ShellView>) -> Div {
    let created = flow.selections().account_created;
    let locale = flow.locale();
    let palette = flow.palette();

    let name_entity = fields.name.clone();
    let password_entity = fields.password.clone();
    let create_click = cx.listener(move |view, _ev, _window, cx| {
        let name = name_entity.read(cx).content.trim().to_string();
        let password = password_entity.read(cx).content.clone();
        if name.is_empty() || password.is_empty() {
            view.oobe_ui.set_account_validation_error(true);
            cx.notify();
            return;
        }
        view.oobe_ui.set_account_validation_error(false);
        if let Some(flow) = view.oobe.as_mut() {
            flow.set_account_created(true);
            crate::oobe::save_state(flow.state());
        }
        cx.notify();
    });

    let mut body = div()
        .flex()
        .flex_col()
        .gap(px(14.))
        .child(labeled_field(t(locale, Key::AccountNameLabel), fields.name.clone(), palette))
        .child(labeled_field(t(locale, Key::AccountPasswordLabel), fields.password.clone(), palette));

    if ui.account_validation_error {
        body = body.child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(palette.destructive, 1.0))
                .child(t(locale, Key::AccountValidationError)),
        );
    }

    body = body.child(widgets::step_button(
        "oobe-create-account",
        if created { t(locale, Key::AccountCreatedButton) } else { t(locale, Key::AccountCreateButton) },
        StepButtonVariant::Primary,
        created,
        palette,
        create_click,
    ));

    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(20.))
        .child(widgets::title(t(locale, Key::AccountTitle), palette))
        .child(widgets::subtitle(t(locale, Key::AccountSubtitle), palette))
        .child(widgets::card(body, palette))
}

fn labeled_field(label: &'static str, field: gpui::Entity<widgets::OobeTextField>, palette: crate::oobe::palette::OobePalette) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(label))
        .child(field)
}
