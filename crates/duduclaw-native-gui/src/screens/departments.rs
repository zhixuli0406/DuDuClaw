// WP-S6b3-P (S6b 第三波, 2026-08-22) — "部門" (`Departments.dc.html`, B1
// 降規二欄: left department list + right detail panel, filling the whole
// content area — no page-level header/breadcrumb artboard of its own). A
// "進階設定" drill-down leaf (`active_page == "departments"`, no `nav.rs`
// entry — wired from `manage_advanced.rs`'s 部門 row by this same pass, see
// that file's own header comment). This page adds one compact breadcrumb bar
// above the two columns (matching every sibling drill-down leaf's own "進階
// 設定 › X" navigability, via `manage_advanced_common::breadcrumb`) — the
// canvas's own artboard has no return-to-index affordance because it is a
// standalone mockup, not evidence that real navigation should be dropped.
//
// ── RPC shape (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, never guessed) ────────────────────────────────────────────
//   `departments.list {}` (dispatch L6379, handler `handle_departments_list`
//   L26380, no `require_*!()` gate) → `{"departments": [DepartmentInfo]}`
//   where `DepartmentInfo` (`crates/duduclaw-gateway/src/departments.rs`
//   L20-30) is `{name, agent_count, members: [String], wiki_pages, skills}`.
//   Already a confirmed-live shape in this crate — `create_agent.rs` calls
//   the same RPC (see that file's own doc comment, L55/L348).
//
// ── Deliberate deviations from the canvas (documented, not silent) ────────
// 1. **No department description.** The canvas's right panel shows a
//    one-line description under the title ("負責訊息回覆、退換貨與客訴分流")
//    — `DepartmentInfo` has no such field anywhere (name/agent_count/
//    members/wiki_pages/skills only). Replaced with a real fact this exact
//    row carries: the member/wiki/skill counts, same "no backing field →
//    show what the RPC actually returns" precedent `users.rs`'s own header
//    comment point (1) already establishes for its dropped 通道身分 column.
// 2. **No per-member role/title.** The canvas's member rows show "組長 ·
//    主要回覆人" / "組員" / "組員 · 兼行銷組" — `members` is `Vec<String>`
//    (display names only), no role/title data anywhere in the schema. Every
//    member row renders just the name, no fabricated title line.
// 3. **＋新增部門 / 編輯 / 移除 are decision-class — assembled, not wired**,
//    same "disabled, no click handler" precedent `users.rs`'s 新增成員/編輯
//    buttons already establish (`departments.create`/`.remove` are real RPCs,
//    dispatch L6383/6387, but writing is out of this page's read-only scope
//    this round).

use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{button, empty_state, skeleton, ButtonVariant};
use crate::rpc::CallError;
use crate::screens::manage_advanced_common::breadcrumb;
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

pub use crate::screens::dashboard::Loadable;

const LIST_WIDTH: f32 = 240.0;

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct DepartmentRow {
    pub name: String,
    pub agent_count: usize,
    pub members: Vec<String>,
    pub wiki_pages: usize,
    pub skills: usize,
}

pub fn parse_departments_list(v: &Value) -> Vec<DepartmentRow> {
    v.get("departments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|d| DepartmentRow {
            name: d.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
            agent_count: d.get("agent_count").and_then(Value::as_u64).unwrap_or(0) as usize,
            members: d
                .get("members")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|m| m.as_str().map(str::to_string))
                .collect(),
            wiki_pages: d.get("wiki_pages").and_then(Value::as_u64).unwrap_or(0) as usize,
            skills: d.get("skills").and_then(Value::as_u64).unwrap_or(0) as usize,
        })
        .collect()
}

// ── State ──────────────────────────────────────────────────────────────

pub struct DepartmentsState {
    requested: bool,
    pub departments: Loadable<Vec<DepartmentRow>>,
    pub selected: usize,
}

impl Default for DepartmentsState {
    fn default() -> Self {
        Self { requested: false, departments: Loadable::Loading, selected: 0 }
    }
}

impl Global for DepartmentsState {}

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if state.active_page != "departments" || cx.default_global::<DepartmentsState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<DepartmentsState>().requested = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "departments.list", json!({}), |cx, result| {
        cx.default_global::<DepartmentsState>().departments = result.map(|v| parse_departments_list(&v)).into();
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

// ── Left column: department list ──────────────────────────────────────

fn dept_row(locale: Locale, row: &DepartmentRow, idx: usize, selected: bool, is_last: bool, cx: &mut Context<RootView>) -> Stateful<Div> {
    let mut r = div()
        .id(SharedString::from(format!("dept-row-{idx}")))
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_between()
        .px_4()
        .py_2p5()
        .when(selected, |el| el.bg(theme::alpha(theme::BRAND, 0.10)))
        .when(!selected, |el| el.hover(|s| s.bg(theme::alpha(theme::SURFACE_HOVER, 1.0))))
        .child(
            div()
                .text_size(px(theme::TEXT_SM))
                .font_weight(if selected { gpui::FontWeight::SEMIBOLD } else { gpui::FontWeight::NORMAL })
                .text_color(theme::alpha(if selected { theme::BRAND } else { theme::FOREGROUND }, 1.0))
                .child(SharedString::from(row.name.clone())),
        )
        .child(
            div()
                .text_size(px(11.5))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::t1(locale, "departments.memberCount", "n", &row.agent_count.to_string())),
        )
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.global_mut::<DepartmentsState>().selected = idx;
            cx.notify();
        }));
    if !is_last {
        r = r.border_b_1().border_color(theme::border());
    }
    r
}

fn list_column(locale: Locale, rows: &[DepartmentRow], selected: usize, cx: &mut Context<RootView>) -> Stateful<Div> {
    let n = rows.len();
    let mut list = div().flex().flex_col();
    for (idx, row) in rows.iter().enumerate() {
        list = list.child(dept_row(locale, row, idx, idx == selected, idx + 1 == n, cx));
    }
    div()
        .id("departments-list-column")
        .w(px(LIST_WIDTH))
        .flex_shrink_0()
        .h_full()
        .overflow_y_scroll()
        .border_r_1()
        .border_color(theme::border())
        .child(list)
}

// ── Right column: detail panel ────────────────────────────────────────

fn member_row(name: &str, is_last: bool) -> Div {
    let mut r = div()
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py_2p5()
        .child(
            div()
                .size(px(28.))
                .flex_shrink_0()
                .rounded_full()
                .bg(theme::alpha(theme::MUTED, 1.0))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(theme::TEXT_XS))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(name.chars().next().map(String::from).unwrap_or_default()),
        )
        .child(div().flex_1().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(name.to_string()));
    if !is_last {
        r = r.border_b_1().border_color(theme::border());
    }
    r
}

fn detail_column(locale: Locale, row: Option<&DepartmentRow>, cx: &mut Context<RootView>) -> Stateful<Div> {
    let Some(row) = row else {
        return div().id("departments-detail-empty").flex_1().flex().items_center().justify_center().child(empty_state(
            "🏢",
            i18n::t(locale, "departments.empty"),
            None,
            None::<Div>,
        ));
    };

    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().text_size(px(17.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(SharedString::from(row.name.clone())))
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(i18n::tn(
                            locale,
                            "departments.summaryLine",
                            &[
                                ("agents", &row.agent_count.to_string()),
                                ("wiki", &row.wiki_pages.to_string()),
                                ("skills", &row.skills.to_string()),
                            ],
                        )),
                ),
        )
        .child(button(
            "dept-edit",
            i18n::t(locale, "departments.edit"),
            ButtonVariant::Secondary,
            true, // decision-type action — assembled, not wired; see module header §3
            None,
            |_ev, _window, _cx| {},
        ));

    let members_body: Div = if row.members.is_empty() {
        div().child(empty_state("👤", i18n::t(locale, "departments.noMembers"), None, None::<Div>))
    } else {
        let n = row.members.len();
        let mut group = div()
            .w_full()
            .flex()
            .flex_col()
            .rounded(px(theme::RADIUS_XL))
            .overflow_hidden()
            .bg(theme::alpha(theme::SURFACE, 1.0))
            .border_1()
            .border_color(theme::surface_border())
            .shadow(theme::surface_shadow());
        for (i, m) in row.members.iter().enumerate() {
            group = group.child(member_row(m, i + 1 == n));
        }
        group
    };

    let members_section = div()
        .mt_5()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(
            div()
                .px_0p5()
                .text_size(px(theme::TEXT_XS))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::t(locale, "departments.members")),
        )
        .child(members_body);

    let _ = cx; // header button's on_click currently ignores cx (assembled-not-wired)
    div().id("departments-detail-column").flex_1().h_full().overflow_y_scroll().p_6().child(header).child(members_section)
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    maybe_fetch(state, cx);
    let locale = state.locale;
    let g = cx.default_global::<DepartmentsState>();
    let departments = g.departments.clone();
    let selected = g.selected;

    let crumb_bar = div()
        .flex_shrink_0()
        .px_6()
        .pt_5()
        .pb_3()
        .child(breadcrumb("departments-breadcrumb", locale, i18n::t(locale, "departments.title"), cx))
        .child(
            div()
                .mt_1()
                .text_size(px(17.))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(i18n::t(locale, "departments.title")),
        );

    let body: Div = match &departments {
        Loadable::Loading => div().flex_1().p_6().child(skeleton(px(700.), px(300.))),
        Loadable::Failed(err) => div().flex_1().p_6().child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(SharedString::from(err.clone()))),
        Loadable::Ready(rows) if rows.is_empty() => {
            div().flex_1().flex().items_center().justify_center().child(empty_state("🏢", i18n::t(locale, "departments.empty"), None, None::<Div>))
        }
        Loadable::Ready(rows) => {
            let selected_idx = selected.min(rows.len().saturating_sub(1));
            div()
                .flex_1()
                .flex()
                .overflow_hidden()
                .child(list_column(locale, rows, selected_idx, cx))
                .child(detail_column(locale, rows.get(selected_idx), cx))
        }
    };

    div().id("departments-page").size_full().flex().flex_col().child(crumb_bar).child(div().flex_1().flex().overflow_hidden().child(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_departments_list_reads_every_field() {
        let v = json!({ "departments": [
            { "name": "客服組", "agent_count": 8, "members": ["小杜", "阿明", "小雅"], "wiki_pages": 4, "skills": 2 },
        ]});
        let rows = parse_departments_list(&v);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "客服組");
        assert_eq!(rows[0].agent_count, 8);
        assert_eq!(rows[0].members, vec!["小杜", "阿明", "小雅"]);
        assert_eq!(rows[0].wiki_pages, 4);
        assert_eq!(rows[0].skills, 2);
    }

    #[test]
    fn parse_departments_list_missing_array_is_empty_not_panicking() {
        assert!(parse_departments_list(&json!({})).is_empty());
        assert!(parse_departments_list(&json!(null)).is_empty());
    }

    #[test]
    fn parse_departments_list_missing_members_is_empty_vec() {
        let v = json!({ "departments": [ { "name": "採購組", "agent_count": 4 } ] });
        let rows = parse_departments_list(&v);
        assert_eq!(rows[0].members, Vec::<String>::new());
    }
}
