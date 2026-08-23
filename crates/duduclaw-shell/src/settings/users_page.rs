// D4b — 使用者.
//
// Two RPCs, both of which already existed — this page adds no backend:
//
//   users.me              -> who is signed in on this machine
//   users.change_password -> self-service rotation of THAT account's password
//
// `users.change_password` is deliberately not admin-gated on the gateway
// (see its own doc comment there): it only ever mutates `ctx.user_id`, so it
// is the one write in this whole app that cannot touch anything but the
// caller. This page therefore never asks which account to change — there is
// exactly one, and it is the one this shell is signed in as.
//
// ── What this page does NOT do ─────────────────────────────────────────
// No user creation, deletion or role editing. Those are `users.create` /
// `users.update` / `users.remove`, which are Enterprise-gated on the gateway
// and belong to the dashboard's multi-user surface; a duty box's local
// screen has one operator standing at it. Adding the buttons here would mean
// rendering controls that answer "此版本不支援" on most installs.

use gpui::{div, prelude::*, px, Context, Div};

use serde_json::Value;

use super::widgets::{self, Tone};
use super::{client, spawn_rpc, Load};
use crate::palette::ShellPalette;
use crate::ShellView;

pub(crate) const ME_METHOD: &str = "users.me";
pub(crate) const CHANGE_PASSWORD_METHOD: &str = "users.change_password";

/// Minimum new-password length, in CHARACTERS.
///
/// The gateway's own floor is 8 BYTES. Counting characters here is
/// deliberately the stricter of the two: eight bytes is three CJK
/// characters, and "至少 8 個字元" is both what the label says and what an
/// operator reading it would expect. A rule the UI states and the UI
/// enforces beats one that silently means something else for half the
/// alphabet.
pub(crate) const MIN_PASSWORD_CHARS: usize = 8;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SignedInUser {
    pub(crate) name: Option<String>,
    pub(crate) email: Option<String>,
    pub(crate) role: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct UsersPageState {
    pub(crate) me: Load<SignedInUser>,
    in_flight: bool,
    /// The last change attempt's outcome, as an already-decided line + tone.
    last_result: Option<(String, bool)>,
    /// A client-side complaint about what was typed. Nothing was sent, so it
    /// is kept apart from `last_result` — describing it as a failed change
    /// would misreport what happened.
    typed_error: Option<&'static str>,
}

impl UsersPageState {
    fn begin(&mut self) -> bool {
        if self.in_flight {
            return false;
        }
        self.in_flight = true;
        self.typed_error = None;
        true
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.in_flight
    }
}

/// Pure: `users.me`'s payload -> [`SignedInUser`]. Tolerant about where the
/// fields sit: the gateway wraps some of its `users.*` answers in a `user`
/// object, so both shapes are read rather than one being assumed.
pub(crate) fn parse_me(payload: &Value) -> SignedInUser {
    let root = payload.get("user").unwrap_or(payload);
    let s = |key: &str| root.get(key).and_then(Value::as_str).map(str::to_string).filter(|v| !v.trim().is_empty());
    SignedInUser { name: s("name"), email: s("email"), role: s("role") }
}

/// zh-TW for the roles this product actually issues. An unknown role is
/// shown verbatim rather than mapped to a guess — a role this build has not
/// heard of is still a fact about the account.
pub(crate) fn role_label(role: &str) -> String {
    match role.trim().to_ascii_lowercase().as_str() {
        "admin" | "owner" => "管理員".to_string(),
        "manager" => "主管".to_string(),
        "user" | "member" => "一般使用者".to_string(),
        "viewer" => "唯讀".to_string(),
        "" => "—".to_string(),
        _ => role.trim().to_string(),
    }
}

/// Pure: the three typed values -> either the request payload or a
/// complaint. Every rule the operator is told about is enforced HERE, before
/// anything leaves the machine; the gateway re-checks its own (byte length,
/// current-password correctness, new≠current) regardless.
pub(crate) fn validate_change(current: &str, new: &str, confirm: &str) -> Result<(String, String), &'static str> {
    if current.is_empty() {
        return Err("請輸入目前的密碼。");
    }
    // Character count, not byte length — see `MIN_PASSWORD_CHARS`.
    if new.chars().count() < MIN_PASSWORD_CHARS {
        return Err("新密碼至少要 8 個字元。");
    }
    if new != confirm {
        return Err("兩次輸入的新密碼不一致。");
    }
    if new == current {
        return Err("新密碼不能和目前的密碼相同。");
    }
    Ok((current.to_string(), new.to_string()))
}

pub(crate) fn ensure_loaded(view: &mut ShellView, cx: &mut Context<ShellView>) {
    if !view.settings_ui.users.me.needs_load() {
        return;
    }
    view.settings_ui.users.me = Load::Loading;
    spawn_rpc(
        cx,
        || client::call(ME_METHOD, serde_json::json!({})),
        |view, result, cx| {
            view.settings_ui.users.me = match result {
                Ok(payload) => Load::Loaded(parse_me(&payload)),
                Err(e) => {
                    eprintln!("[settings/users] {ME_METHOD} failed: {e:?}");
                    Load::Failed(e)
                }
            };
            cx.notify();
        },
    );
}

fn submit_change(view: &mut ShellView, cx: &mut Context<ShellView>) {
    if view.settings_ui.users.is_busy() {
        return;
    }
    let current = view.settings_fields.current_password.read(cx).content(cx);
    let new = view.settings_fields.new_password.read(cx).content(cx);
    let confirm = view.settings_fields.confirm_password.read(cx).content(cx);

    let (current, new) = match validate_change(&current, &new, &confirm) {
        Ok(pair) => pair,
        Err(complaint) => {
            view.settings_ui.users.typed_error = Some(complaint);
            cx.notify();
            return;
        }
    };
    if !view.settings_ui.users.begin() {
        return;
    }
    view.settings_ui.users.last_result = None;
    cx.notify();
    spawn_rpc(
        cx,
        move || client::call(CHANGE_PASSWORD_METHOD, serde_json::json!({ "current_password": current, "new_password": new })),
        |view, result, cx| {
            view.settings_ui.users.in_flight = false;
            match result {
                Ok(_) => {
                    view.settings_ui.users.last_result = Some(("密碼已更新。".to_string(), true));
                    // Plaintext must not keep sitting in three text fields
                    // after a successful rotation — the same reasoning the
                    // lockscreen's own password field applies.
                    view.settings_fields.clear_passwords(cx);
                }
                Err(e) => {
                    // NEVER log the error's context here beyond its kind:
                    // this call's params are two passwords.
                    eprintln!("[settings/users] {CHANGE_PASSWORD_METHOD} failed");
                    view.settings_ui.users.last_result = Some((e.user_message(), false));
                }
            }
            cx.notify();
        },
    );
}

// ── Render ───────────────────────────────────────────────────────────────

pub(crate) fn render(
    body: Div,
    state: &UsersPageState,
    fields: &crate::oobe::SettingsFields,
    palette: ShellPalette,
    cx: &mut Context<ShellView>,
) -> Div {
    cx.spawn(async move |weak, cx| {
        let _ = weak.update(cx, ensure_loaded);
    })
    .detach();

    body.child(account_card(state, palette, cx)).child(password_card(state, fields, palette, cx))
}

fn account_card(state: &UsersPageState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let refresh = widgets::button(
        "settings-users-refresh",
        "重新整理".to_string(),
        widgets::ButtonWeight::Secondary,
        !matches!(state.me, Load::Loading),
        palette,
        cx.listener(|view, _ev, _window, cx| {
            view.settings_ui.users.me = Load::NotLoaded;
            ensure_loaded(view, cx);
            cx.notify();
        }),
    );
    let card = widgets::card(palette).child(widgets::card_header("目前登入的帳號", Some(refresh.into_any_element()), palette));
    match &state.me {
        Load::NotLoaded | Load::Loading => card.child(widgets::notice_static("讀取中…", Tone::Muted, palette)),
        Load::Failed(e) => card.child(widgets::notice(e.user_message(), Tone::Danger, palette)),
        Load::Loaded(me) => card
            .child(widgets::value_row("名稱", me.name.clone().unwrap_or_else(|| "—".to_string()), palette))
            .child(widgets::value_row("帳號", me.email.clone().unwrap_or_else(|| "—".to_string()), palette))
            .child(widgets::value_row("權限", me.role.as_deref().map(role_label).unwrap_or_else(|| "—".to_string()), palette)),
    }
}

fn password_card(
    state: &UsersPageState,
    fields: &crate::oobe::SettingsFields,
    palette: ShellPalette,
    cx: &mut Context<ShellView>,
) -> Div {
    let enabled = !state.is_busy();
    let mut card = widgets::card(palette)
        .child(widgets::card_header("變更密碼", None, palette))
        .child(labeled("目前密碼", fields.current_password.clone(), palette))
        .child(labeled("新密碼（至少 8 個字元）", fields.new_password.clone(), palette))
        .child(labeled("再輸入一次新密碼", fields.confirm_password.clone(), palette))
        .child(
            div().flex().items_center().gap(px(10.)).child(widgets::button(
                "settings-users-change-password",
                if state.is_busy() { "變更中…".to_string() } else { "變更密碼".to_string() },
                widgets::ButtonWeight::Primary,
                enabled,
                palette,
                cx.listener(|view, _ev, _window, cx| submit_change(view, cx)),
            )),
        );
    if let Some(complaint) = state.typed_error {
        card = card.child(widgets::notice_static(complaint, Tone::Danger, palette));
    }
    if let Some((message, ok)) = &state.last_result {
        card = card.child(widgets::notice(message.clone(), if *ok { Tone::Success } else { Tone::Danger }, palette));
    }
    card.child(widgets::notice_static("這組密碼同時用於解鎖這台機器與登入管理介面。", Tone::Muted, palette))
}

fn labeled(label: &'static str, field: gpui::Entity<crate::oobe::SettingsTextField>, palette: ShellPalette) -> Div {
    div().flex().flex_col().gap(px(4.)).child(widgets::field_label(label, palette)).child(field)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_flat_payload_and_a_wrapped_one_both_parse() {
        let flat = parse_me(&json!({ "name": "Louis", "email": "louis@example.com", "role": "admin" }));
        assert_eq!(flat.name.as_deref(), Some("Louis"));
        let wrapped = parse_me(&json!({ "user": { "name": "Louis", "email": "louis@example.com", "role": "admin" } }));
        assert_eq!(wrapped, flat, "both gateway shapes must read the same");
    }

    #[test]
    fn absent_and_blank_fields_become_none_rather_than_empty_rows() {
        let me = parse_me(&json!({ "name": "   " }));
        assert_eq!(me.name, None);
        assert_eq!(me.email, None);
        assert_eq!(me.role, None);
    }

    #[test]
    fn known_roles_are_translated_and_unknown_ones_are_shown_verbatim() {
        assert_eq!(role_label("admin"), "管理員");
        assert_eq!(role_label("Manager"), "主管");
        assert_eq!(role_label("viewer"), "唯讀");
        assert_eq!(role_label("auditor"), "auditor", "an unrecognised role is still a fact, not a guess");
        assert_eq!(role_label("  "), "—");
    }

    #[test]
    fn a_valid_change_produces_the_pair_to_send() {
        assert_eq!(
            validate_change("old-secret", "brand-new-secret", "brand-new-secret"),
            Ok(("old-secret".to_string(), "brand-new-secret".to_string()))
        );
    }

    #[test]
    fn each_rule_the_ui_states_is_enforced_with_its_own_complaint() {
        let empty_current = validate_change("", "brand-new-secret", "brand-new-secret");
        let too_short = validate_change("old-secret", "short", "short");
        let mismatch = validate_change("old-secret", "brand-new-secret", "brand-new-secre");
        let same = validate_change("old-secret", "old-secret", "old-secret");
        for r in [&empty_current, &too_short, &mismatch, &same] {
            assert!(r.is_err(), "{r:?} should have been refused");
        }
        // Four different problems must not collapse onto one message.
        let mut messages = vec![
            empty_current.unwrap_err(),
            too_short.unwrap_err(),
            mismatch.unwrap_err(),
            same.unwrap_err(),
        ];
        messages.sort_unstable();
        messages.dedup();
        assert_eq!(messages.len(), 4);
    }

    /// The length rule is stated in CHARACTERS, so it has to be enforced in
    /// characters — eight bytes of CJK is three characters and must not pass.
    #[test]
    fn the_length_floor_counts_characters_not_bytes() {
        assert!(validate_change("old-secret", "密碼一二", "密碼一二").is_err(), "4 CJK chars (12 bytes) is under the stated floor");
        let eight_cjk = "密碼一二三四五六";
        assert_eq!(eight_cjk.chars().count(), MIN_PASSWORD_CHARS);
        assert!(validate_change("old-secret", eight_cjk, eight_cjk).is_ok());
    }

    #[test]
    fn only_one_change_may_be_in_flight() {
        let mut state = UsersPageState::default();
        assert!(state.begin());
        assert!(!state.begin());
    }

    #[test]
    fn beginning_a_change_clears_the_previous_typed_complaint() {
        let mut state = UsersPageState::default();
        state.typed_error = Some("兩次輸入的新密碼不一致。");
        assert!(state.begin());
        assert_eq!(state.typed_error, None);
    }

    #[test]
    fn a_fresh_page_has_asked_nothing() {
        let state = UsersPageState::default();
        assert!(state.me.needs_load());
        assert!(!state.is_busy());
        assert!(state.last_result.is_none());
    }
}
