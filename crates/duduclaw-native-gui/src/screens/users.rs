// WP-S6b1-L (S6b 第一波) — "成員" (`UsersPage.dc.html`, B19) — the "四大 OS
// 帳號管理一致式" skeleton this wave's brief calls for (macOS Users & Groups /
// Windows 其他使用者 / GNOME Settings Users style: one flat account list,
// role badge, no per-row edit form). No `nav.rs` entry of its own this
// round — reached via `screens::manage_advanced`'s 成員 row (wired this same
// pass, see that module's own header comment) or the debug-page override
// (`DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=users`); self-attached in `shell.rs` per
// the "D 先掛好分支就直接可達，未掛就自己掛" precedent that file's own
// comment blocks already establish for every S5b3-wave page.
//
// Visual authority: `commercial/design/duduclaw-s6-biz-pages/UsersPage.dc.
// html` — breadcrumb (進階設定 › 成員) → header (title/subtitle + ＋新增成員)
// → boxed table → footer note, in that order. Per this task's own "版面禁抄
// web" rule, `web/src/pages/UsersPage.tsx` (dropdown menus, 5 sub-dialogs,
// per-row unbind chips) is a functional RPC cross-reference only, never a
// layout source.
//
// ── RPC shape (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, never guessed) ────────────────────────────────────────────
//   `users.list` (dispatch match arm L6391, handler `handle_users_list`
//   L26429, `require_admin!()`) → `{"users": [{"id","email","display_name",
//   "role","status","department","created_at","updated_at","last_login",
//   "bindings": [{"user_id","agent_name","access_level","bound_at"}]}]}`.
//   `role` is one of `employee`/`manager`/`admin`
//   (`web/src/pages/UsersPage.tsx::roleOptions`, mirrored nowhere else on
//   the backend as an enum). `status` is the real 3-value
//   `duduclaw_auth::models::UserStatus` (`Active`/`Suspended`/`Offboarded`,
//   `Display` lowercases to exactly these three strings) — see deviation
//   (2) below. "員工數" reads `bindings.len()` (bound AI agents), not a
//   separate field.
//
// ── Deliberate deviations from the canvas, documented not silent ─────────
// 1. **通道身分 column dropped entirely.** The canvas's "已綁定/未綁定"
//    column reads `GET /api/channel-identity/list`
//    (`web/src/lib/channel-identity-api.ts`) — a REST endpoint, not a `/ws`
//    RPC method. This crate's `rpc.rs` only ever speaks the `/ws` `WsFrame`
//    protocol (see that module's own header comment) — there is no HTTP
//    client wired into this crate to reach it, and wiring one is out of
//    this task's "users.* RPC" scope. Rather than fabricate a boolean, this
//    page drops the column outright — same "no backing field, drop the
//    line" precedent `accounts.rs`'s own header comment point (1) already
//    establishes for the canvas/data mismatch it hit.
// 2. **Status labels are the real 3-value enum, not the canvas's 3 example
//    strings.** The canvas's example rows show 啟用中/待驗證/已離職, but
//    `UserStatus` only ever serializes `active`/`suspended`/`offboarded` —
//    there is no "pending verification" status a real user can be in. This
//    page labels the three REAL values (啟用中/已停用/已離職) instead of
//    reproducing the canvas's fictional middle state.
// 3. **Footer note trimmed of its one functional claim.** The canvas's
//    footer text opens with "角色可在列內直接切換" (role is directly
//    switchable in-row) — this skeleton pass has no such control (see (4)
//    below), so claiming it would be a UI lie, not a policy statement. Only
//    the offboarding-process guidance half is kept.
// 4. **新增成員／編輯／移除 are all decision-type actions — assembled, not
//    wired**, per this task's own brief ("新增/停用決策類組裝不真按"). All
//    three render as `mds_gpui::button`s with `disabled: true` (which, per
//    that primitive's own doc comment, attaches NO click handler at all) —
//    same established pattern `mcp_keys.rs`'s create/revoke actions and
//    `accounts.rs`'s "新增帳號" already use.

use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, button, empty_state, skeleton, BadgeVariant, ButtonVariant};
use crate::rpc::CallError;
use crate::screens::settings_common::boxed_group;
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

pub use crate::screens::dashboard::Loadable;

const CONTENT_MAX_WIDTH: f32 = 860.0;

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct UserRow {
    pub id: String,
    pub display_name: String,
    pub role: String,
    pub department: Option<String>,
    pub status: String,
    pub agent_count: usize,
}

pub fn parse_users_list(v: &Value) -> Vec<UserRow> {
    v.get("users")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|u| UserRow {
            id: u.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
            display_name: u.get("display_name").and_then(Value::as_str).unwrap_or("").to_string(),
            role: u.get("role").and_then(Value::as_str).unwrap_or("").to_string(),
            department: u
                .get("department")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            status: u.get("status").and_then(Value::as_str).unwrap_or("").to_string(),
            agent_count: u.get("bindings").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
        })
        .collect()
}

// ── State ──────────────────────────────────────────────────────────────

pub struct UsersState {
    requested: bool,
    pub users: Loadable<Vec<UserRow>>,
}

impl Default for UsersState {
    fn default() -> Self {
        Self { requested: false, users: Loadable::Loading }
    }
}

impl Global for UsersState {}

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if state.active_page != "users" || cx.default_global::<UsersState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<UsersState>().requested = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "users.list", json!({}), |cx, result| {
        cx.default_global::<UsersState>().users = result.map(|v| parse_users_list(&v)).into();
    });
}

fn spawn_call(
    cx: &mut Context<RootView>,
    session_tx: tokio_mpsc::UnboundedSender<SessionCommand>,
    method: &'static str,
    params: Value,
    apply: impl FnOnce(&mut Context<RootView>, Result<Value, String>) + 'static,
) {
    cx.spawn(async move |weak, cx| {
        let rx = ws_status::call(&session_tx, method, params);
        let outcome = match rx.await {
            Ok(Ok(payload)) => Ok(payload),
            Ok(Err(err)) => Err(describe_call_error(&err)),
            Err(_) => Err("背景連線執行緒已結束".to_string()),
        };
        let _ = weak.update(cx, |_view, cx| {
            apply(cx, outcome);
            cx.notify();
        });
    })
    .detach();
}

fn describe_call_error(e: &CallError) -> String {
    match e {
        CallError::NotConnected => "尚未連線到伺服器".to_string(),
        CallError::Timeout => "請求逾時".to_string(),
        CallError::Disconnected => "連線已中斷".to_string(),
        CallError::Rejected(v) => v
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| v.as_str().map(str::to_string))
            .unwrap_or_else(|| v.to_string()),
    }
}

// ── Breadcrumb (2-segment: 進階設定 › 成員 — matches the canvas exactly;
// `settings_common::breadcrumb` is hardcoded to "整合" for a different S5b1-C
// batch, so this page carries its own small local version rather than
// widening that shared helper's signature for one caller — same "duplicate,
// don't couple two batches through a shared module" reasoning that module's
// own header comment already applies to itself). ─────────────────────────

fn breadcrumb(locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    div()
        .id("users-breadcrumb")
        .flex()
        .items_center()
        .gap_1p5()
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(
            div()
                .id("users-breadcrumb-root")
                .cursor_pointer()
                .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
                .child(i18n::t(locale, "nav.manageAdvanced"))
                .on_click(cx.listener(|this, _ev, _window, cx| {
                    this.active_page = "manageAdvanced";
                    cx.notify();
                })),
        )
        .child(SharedString::from("›"))
        .child(div().overflow_hidden().child(i18n::t(locale, "users.title")))
}

// ── Display helpers ────────────────────────────────────────────────────

/// Label is the RAW `role` string, not a translated word — matching both
/// the canvas (its example badges literally read "admin"/"manager"/
/// "employee") and `web/src/pages/UsersPage.tsx`'s `<Badge>{user.role}</
/// Badge>` (no i18n lookup at all). Only the badge COLOR is derived from
/// the role, same as the canvas's admin=red/manager=amber/employee=gray
/// palette.
fn role_badge(_locale: Locale, role: &str) -> Div {
    let variant = match role {
        "admin" => BadgeVariant::Destructive,
        "manager" => BadgeVariant::Warning,
        "employee" => BadgeVariant::Secondary,
        _ => BadgeVariant::Outline,
    };
    badge(SharedString::from(role.to_string()), variant)
}

/// `(dot color, label)` for the real 3-value `UserStatus` enum — see this
/// module's own header comment point (2) for why this is NOT the canvas's
/// 3 example strings.
fn status_display(locale: Locale, status: &str) -> (u32, SharedString) {
    match status {
        "active" => (theme::SUCCESS, i18n::t(locale, "users.status.active")),
        "suspended" => (theme::WARNING, i18n::t(locale, "users.status.suspended")),
        "offboarded" => (theme::MUTED_FOREGROUND, i18n::t(locale, "users.status.offboarded")),
        other => (theme::MUTED_FOREGROUND, SharedString::from(other.to_string())),
    }
}

// ── Table ──────────────────────────────────────────────────────────────

fn header_row(locale: Locale) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py_2()
        .bg(theme::alpha(theme::MUTED, 0.35))
        .text_size(px(11.))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(div().flex_1().child(i18n::t(locale, "users.col.name")))
        .child(div().w(px(90.)).flex_shrink_0().child(i18n::t(locale, "users.col.role")))
        .child(div().w(px(60.)).flex_shrink_0().child(i18n::t(locale, "users.col.agents")))
        .child(div().w(px(90.)).flex_shrink_0().child(i18n::t(locale, "users.col.status")))
        .child(div().w(px(70.)).flex_shrink_0().child(""))
}

fn user_row(locale: Locale, u: &UserRow, is_last: bool) -> Div {
    let (dot_color, status_label) = status_display(locale, &u.status);

    let name_col = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .child(
            div()
                .text_size(px(theme::TEXT_SM))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(SharedString::from(u.display_name.clone())),
        )
        .children(u.department.clone().map(|d| {
            div().text_size(px(10.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(SharedString::from(d))
        }));

    let status_col = div()
        .w(px(90.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap_1p5()
        .child(div().size(px(7.)).rounded_full().bg(theme::alpha(dot_color, 1.0)))
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(status_label));

    let actions_col = div().w(px(70.)).flex_shrink_0().child(button(
        SharedString::from(format!("users-edit-{}", u.id)),
        i18n::t(locale, "users.action.edit"),
        ButtonVariant::Ghost,
        true, // decision-type action — assembled, not wired; see module header §4
        None,
        |_ev, _window, _cx| {},
    ));

    let mut row = div()
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py_2p5()
        .child(name_col)
        .child(div().w(px(90.)).flex_shrink_0().child(role_badge(locale, &u.role)))
        .child(div().w(px(60.)).flex_shrink_0().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(u.agent_count.to_string()))
        .child(status_col)
        .child(actions_col);
    if !is_last {
        row = row.border_b_1().border_color(theme::border());
    }
    row
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    maybe_fetch(state, cx);
    let locale = state.locale;
    let users = cx.default_global::<UsersState>().users.clone();

    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().text_size(px(theme::TEXT_XL)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "users.title")))
                .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "users.subtitle"))),
        )
        .child(button(
            "users-create",
            i18n::t(locale, "users.create"),
            ButtonVariant::Primary,
            true, // decision-type action — assembled, not wired; see module header §4
            None,
            |_ev, _window, _cx| {},
        ));

    let body = match &users {
        Loadable::Loading => div().flex().flex_col().gap_2().child(skeleton(px(760.), px(48.))).child(skeleton(px(760.), px(48.))).child(skeleton(px(760.), px(48.))),
        Loadable::Failed(err) => div().p_4().child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(SharedString::from(err.clone()))),
        Loadable::Ready(rows) if rows.is_empty() => div().child(empty_state("👤", i18n::t(locale, "users.empty"), None, None::<Div>)),
        Loadable::Ready(rows) => {
            let n = rows.len();
            boxed_group(
                std::iter::once(header_row(locale))
                    .chain(rows.iter().enumerate().map(|(i, u)| user_row(locale, u, i + 1 == n)))
                    .collect(),
            )
        }
    };

    let footer = div()
        .text_size(px(11.5))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .text_center()
        .child(i18n::t(locale, "users.footerNote"));

    div()
        .id("users-page")
        .size_full()
        .overflow_y_scroll()
        .child(
            div()
                .max_w(px(CONTENT_MAX_WIDTH))
                .mx_auto()
                .flex()
                .flex_col()
                .gap_3p5()
                .p_6()
                .child(breadcrumb(locale, cx))
                .child(header)
                .child(body)
                .child(footer),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_users_list_reads_every_field() {
        let v = json!({ "users": [
            { "id": "u1", "display_name": "陳雅婷", "role": "admin", "status": "active",
              "department": "業務部", "bindings": [ { "agent_name": "a" }, { "agent_name": "b" } ] },
            { "id": "u2", "display_name": "林志遠", "role": "manager", "status": "suspended",
              "department": "", "bindings": [] },
        ]});
        let rows = parse_users_list(&v);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].display_name, "陳雅婷");
        assert_eq!(rows[0].role, "admin");
        assert_eq!(rows[0].department.as_deref(), Some("業務部"));
        assert_eq!(rows[0].agent_count, 2);
        // Empty-string department normalizes to None, not Some("").
        assert_eq!(rows[1].department, None);
        assert_eq!(rows[1].agent_count, 0);
    }

    #[test]
    fn parse_users_list_missing_array_is_empty_not_panicking() {
        assert!(parse_users_list(&json!({})).is_empty());
        assert!(parse_users_list(&json!(null)).is_empty());
    }

    #[test]
    fn status_display_covers_the_real_three_value_enum_and_falls_back() {
        assert_eq!(status_display(Locale::ZhTw, "active").0, theme::SUCCESS);
        assert_eq!(status_display(Locale::ZhTw, "suspended").0, theme::WARNING);
        assert_eq!(status_display(Locale::ZhTw, "offboarded").0, theme::MUTED_FOREGROUND);
        let (color, label) = status_display(Locale::ZhTw, "weird");
        assert_eq!(color, theme::MUTED_FOREGROUND);
        assert_eq!(label.as_ref(), "weird");
    }

    #[test]
    fn describe_call_error_prefers_structured_message_over_bare_string() {
        let msg = describe_call_error(&CallError::Rejected(json!({"code": "denied", "message": "權限不足"})));
        assert_eq!(msg, "權限不足");
        let msg2 = describe_call_error(&CallError::Rejected(json!("連線失敗")));
        assert_eq!(msg2, "連線失敗");
    }
}
