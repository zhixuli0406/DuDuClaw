// WP-S6b1-K (S6b 第一波, 2026-08-21) — "治理規則" (`GovernancePage.dc.html`,
// B18). A "進階設定" drill-down leaf (`active_page == "governance"`, no
// `nav.rs` entry of its own — same "進階設定 高亮，不佔側欄獨立列" convention
// every S5b1-C/S5b2-F drill-down leaf already establishes), reached from
// `manage_advanced.rs`'s 治理 row (that file is a parallel WP's territory
// per this task's own "manage_advanced.rs 歸 L" boundary — this pass only
// self-attaches its own `shell.rs` branch, same "D 先掛好分支就直接可達，未
// 掛就自己掛" precedent every prior wave's own doc comments already cite).
//
// This module also owns `GovernanceShell` — the two-tab shell wrapping this
// page (治理規則) and `wiki_trust.rs` (Wiki 信任), per the S6 canvas cover's
// own "Tabs 殼容器覆蓋方式" note (`commercial/design/duduclaw-s6-biz-pages/
// Main.dc.html`): no separate shell artboard exists, each of the two real
// pages draws the identical tab row at its own top. `shell_tabs`/
// `breadcrumb`/`spawn_call`/`describe_call_error` below are therefore
// `pub(super)` and imported by `wiki_trust.rs` too — the same "share within
// one batch, duplicate across batches" precedent `catalog_common.rs`'s own
// header comment documents for its five sibling pages (this WP's two pages
// are exactly that case: one design batch, zero divergence pressure).
//
// Visual authority: `commercial/design/duduclaw-s6-biz-pages/GovernancePage.
// dc.html` — breadcrumb → GovernanceShell tabs → header (title/subtitle + ＋
// 新增規則) → policy-type filter chips (全部/rate/permission/quota/
// lifecycle) → boxed policy table (規則編號/類型/適用對象/內容摘要/開關).
//
// ── RPC shape (read directly from `crates/duduclaw-gateway/src/handlers.
// rs`, never guessed) ──────────────────────────────────────────────────
//   `governance.list` (dispatch ~L5573, handler `handle_governance_list`
//   ~L10503, `require_admin!()`) — params `{agent_id?}` (omitted here: this
//   page always lists every scope, matching the canvas's flat "全部 (12)"
//   table, not a per-agent drill-down) → `{"policies": [{"scope",
//   "policy_type","policy_id","agent_id", ...per-type fields}]}`. Per-type
//   fields (verified against `gov_validate_policy` ~L2137 / the four
//   `defaultPolicy()` shapes `web/src/pages/GovernancePage.tsx` already
//   documents): rate→`resource,limit,window_seconds,action_on_violation`;
//   permission→`allowed_scopes[],denied_scopes[],requires_approval[]`;
//   quota→`daily_token_budget,max_concurrent_tasks,max_memory_entries,
//   reset_cron`; lifecycle→`max_idle_hours,health_check_interval_seconds,
//   auto_suspend_on_violation_count`.
//   `governance.upsert`/`governance.remove` (~L5577/~L5581) both exist and
//   are real, but this task's brief is explicit — "規則啟停/編輯決策類組裝不
//   真按": the header's ＋新增規則 button and every row's toggle switch
//   render fully assembled (correct copy, correct placement) but attach NO
//   click handler, same established pattern `mcp_keys.rs`'s create/revoke
//   buttons already use.
//
// ── Deviations from the canvas (documented, not silent) ──────────────────
// 1. Row toggle switch — `GovPolicy` has NO `enabled`/`disabled` field
//    anywhere in its schema (confirmed reading `gov_validate_policy`'s full
//    field list — every policy that exists IS in effect; there is no
//    separate on/off bit to reflect). The canvas's per-row on/off pattern
//    (3 on, 1 off) is therefore decorative mockup detail with nothing real
//    behind it. Rather than fabricate a fake per-row enabled/disabled fact,
//    every row renders the SAME muted, non-color-coded toggle shape (see
//    `static_toggle` below) — knob resting on the right (an existing,
//    listed policy IS currently applied), always in the same dimmed color
//    `mds_gpui::button`'s own `disabled: true` path already uses for every
//    other inert control in this crate, never the canvas's brand-blue "on"
//    look (that would misrepresent a live fact this schema doesn't carry).
// 2. The canvas's footer caption "切換開關即時生效，不需要另外儲存" is DROPPED
//    — it is a literal functional claim about the (inert) toggle above it;
//    keeping it would misrepresent a non-wired control as live, which this
//    crate's honesty convention (see `manage_advanced.rs`'s own "an
//    inert-looking control is honest; a control that silently does nothing
//    is a worse lie" note) rules out.

use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{tabs, BadgeVariant, TabItem};
use crate::rpc::CallError;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

pub use crate::screens::dashboard::Loadable;

// ── Data model ─────────────────────────────────────────────────────────

pub(super) const POLICY_TYPES: [&str; 4] = ["rate", "permission", "quota", "lifecycle"];

#[derive(Debug, Clone, PartialEq)]
pub struct GovPolicyRow {
    pub scope: String,
    pub policy_type: String,
    pub policy_id: String,
    pub agent_id: String,
    /// Pre-formatted "內容摘要" text — mirrors `web/src/pages/
    /// GovernancePage.tsx::policyDetail()` exactly (same per-type field
    /// composition), built once at parse time so the render layer never
    /// re-reads raw JSON.
    pub detail: String,
}

fn policy_detail(p: &Value) -> String {
    let get_i = |k: &str| p.get(k).and_then(Value::as_i64).unwrap_or(0);
    let get_s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let get_len = |k: &str| p.get(k).and_then(Value::as_array).map(|a| a.len()).unwrap_or(0);
    match p.get("policy_type").and_then(Value::as_str).unwrap_or("") {
        "rate" => format!(
            "{} ≤ {}/{}s → {}",
            get_s("resource"),
            get_i("limit"),
            get_i("window_seconds"),
            get_s("action_on_violation")
        ),
        "permission" => format!("+{} / -{}", get_len("allowed_scopes"), get_len("denied_scopes")),
        "quota" => format!("{} tok/day · {} tasks", get_i("daily_token_budget"), get_i("max_concurrent_tasks")),
        "lifecycle" => format!("idle {}h · hc {}s", get_i("max_idle_hours"), get_i("health_check_interval_seconds")),
        _ => String::new(),
    }
}

pub fn parse_governance_list(v: &Value) -> Vec<GovPolicyRow> {
    v.get("policies")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|p| {
                    let policy_type = p.get("policy_type")?.as_str()?.to_string();
                    let policy_id = p.get("policy_id").and_then(Value::as_str).unwrap_or("").to_string();
                    let agent_id = p.get("agent_id").and_then(Value::as_str).unwrap_or("*").to_string();
                    let scope = p.get("scope").and_then(Value::as_str).unwrap_or(&agent_id).to_string();
                    let detail = policy_detail(p);
                    Some(GovPolicyRow { scope, policy_type, policy_id, agent_id, detail })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn type_badge_variant(policy_type: &str) -> BadgeVariant {
    // Exact canvas palette (GovernancePage.dc.html row chips), NOT the web
    // page's own arbitrary secondary/outline/warning/success mix — visual
    // fidelity to the approved canvas wins per this task's priority rule.
    match policy_type {
        "rate" => BadgeVariant::Info,
        "permission" => BadgeVariant::Destructive,
        "quota" => BadgeVariant::Warning,
        "lifecycle" => BadgeVariant::Success,
        _ => BadgeVariant::Secondary,
    }
}

fn scope_label(row: &GovPolicyRow) -> SharedString {
    if row.scope == "global" || row.agent_id == "*" {
        "*".into()
    } else {
        row.agent_id.clone().into()
    }
}

// ── State ──────────────────────────────────────────────────────────────

pub struct GovernanceState {
    requested: bool,
    pub policies: Loadable<Vec<GovPolicyRow>>,
    /// `None` = "全部"; `Some(t)` = one of `POLICY_TYPES`.
    pub filter: Option<&'static str>,
}

impl GovernanceState {
    fn new() -> Self {
        Self { requested: false, policies: Loadable::Loading, filter: None }
    }
}

impl Global for GovernanceState {}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<GovernanceState>() {
        cx.set_global(GovernanceState::new());
    }
}

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if cx.global::<GovernanceState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<GovernanceState>().requested = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "governance.list", json!({}), |cx, result| {
        cx.global_mut::<GovernanceState>().policies = result.map(|v| parse_governance_list(&v)).into();
    });
}

// ── RPC round-trip boilerplate — shared with `wiki_trust.rs`, same batch
// (see this file's own header comment for why sharing is right here). ────

pub(super) fn spawn_call(
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

pub(super) fn describe_call_error(e: &CallError) -> String {
    match e {
        CallError::NotConnected => "尚未連線到伺服器".to_string(),
        CallError::Timeout => "請求逾時".to_string(),
        CallError::Disconnected => "連線已中斷".to_string(),
        CallError::Rejected(v) => v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()),
    }
}

// ── Breadcrumb + GovernanceShell tabs — shared with `wiki_trust.rs` ──────

/// "進階設定 › 治理" — the second segment is a fixed literal for BOTH pages
/// in this shell (the canvas's own breadcrumb text is identical on
/// `GovernancePage.dc.html` and `WikiTrustPage.dc.html`; which sub-page is
/// active is signalled by `shell_tabs` below, not the breadcrumb). Clicking
/// "進階設定" jumps back to `manageAdvanced`.
pub(super) fn breadcrumb(id_prefix: &'static str, locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    div()
        .id(id_prefix)
        .flex()
        .items_center()
        .gap_1p5()
        .text_size(px(crate::theme::TEXT_XS))
        .text_color(crate::theme::alpha(crate::theme::MUTED_FOREGROUND, 1.0))
        .child(
            div()
                .id(format!("{id_prefix}-root"))
                .cursor_pointer()
                .hover(|s| s.text_color(crate::theme::alpha(crate::theme::FOREGROUND, 1.0)))
                .child(i18n::t(locale, "nav.manageAdvanced"))
                .on_click(cx.listener(|this, _ev, _window, cx| {
                    this.active_page = "manageAdvanced";
                    cx.notify();
                })),
        )
        .child(SharedString::from("›"))
        .child(div().overflow_hidden().child(i18n::t(locale, "governance.breadcrumb")))
}

/// The two-tab row itself — `active` is `"governance"` or `"wikiTrust"`
/// (this crate's own `active_page` ids, see this file's module doc
/// comment).
pub(super) fn shell_tabs(locale: Locale, active: &'static str, cx: &mut Context<RootView>) -> Div {
    let items = vec![
        TabItem::new(
            "governance",
            i18n::t(locale, "governance.tab.rules"),
            cx.listener(|this, _ev, _window, cx| {
                this.active_page = "governance";
                cx.notify();
            }),
        ),
        TabItem::new(
            "wikiTrust",
            i18n::t(locale, "governance.tab.wikiTrust"),
            cx.listener(|this, _ev, _window, cx| {
                this.active_page = "wikiTrust";
                cx.notify();
            }),
        ),
    ];
    div().w_full().child(tabs(items, active))
}

/// A non-interactive toggle pill for decision-class controls this pass
/// assembles but does not wire — same "always dimmed, no click handler"
/// honesty convention `mds_gpui::button`'s own `disabled: true` path already
/// establishes (its `disabled` branch forces a MUTED background regardless
/// of the requested variant). Shared with `security.rs`'s own emergency
/// toggle row (same rationale, different page — see that module's header
/// comment for why it keeps its own tiny copy rather than importing this
/// one: the two pages are a different design-authority pairing than this
/// module's `wiki_trust.rs` sibling).
pub(super) fn static_toggle(knob_right: bool) -> Div {
    div()
        .relative()
        .w(px(34.))
        .h(px(20.))
        .flex_shrink_0()
        .rounded_full()
        .bg(crate::theme::alpha(crate::theme::MUTED, 0.6))
        .child(
            div()
                .absolute()
                .top(px(2.))
                .left(if knob_right { px(16.) } else { px(2.) })
                .size(px(16.))
                .rounded_full()
                .bg(crate::theme::alpha(0xffffff, 0.9)),
        )
}

// ── Filter chips ───────────────────────────────────────────────────────

fn filter_label(locale: Locale, t: Option<&'static str>) -> SharedString {
    match t {
        None => i18n::t(locale, "governance.filter.all"),
        Some("rate") => i18n::t(locale, "governance.filter.rate"),
        Some("permission") => i18n::t(locale, "governance.filter.permission"),
        Some("quota") => i18n::t(locale, "governance.filter.quota"),
        Some("lifecycle") => i18n::t(locale, "governance.filter.lifecycle"),
        Some(other) => other.to_string().into(),
    }
}

fn filter_chip(locale: Locale, t: Option<&'static str>, count: usize, selected: bool, cx: &mut Context<RootView>) -> Stateful<Div> {
    let label: SharedString = format!("{} ({count})", filter_label(locale, t)).into();
    let id: SharedString = format!("gov-filter-{}", t.unwrap_or("all")).into();
    div()
        .id(id)
        .h(px(28.))
        .px_3()
        .flex()
        .items_center()
        .rounded(px(crate::theme::RADIUS_4XL))
        .cursor_pointer()
        .text_size(px(crate::theme::TEXT_XS))
        .font_weight(gpui::FontWeight::MEDIUM)
        .when(selected, |el| {
            el.bg(crate::theme::alpha(crate::theme::BRAND, 1.0)).text_color(crate::theme::alpha(crate::theme::BRAND_FOREGROUND, 1.0))
        })
        .when(!selected, |el| {
            el.bg(crate::theme::alpha(crate::theme::SURFACE, 1.0))
                .border_1()
                .border_color(crate::theme::surface_border())
                .text_color(crate::theme::alpha(crate::theme::MUTED_FOREGROUND, 1.0))
                .hover(|s| s.text_color(crate::theme::alpha(crate::theme::FOREGROUND, 1.0)))
        })
        .child(label)
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.global_mut::<GovernanceState>().filter = t;
            cx.notify();
        }))
}

// ── Rows ───────────────────────────────────────────────────────────────

fn header_row(locale: Locale) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py_2()
        .bg(crate::theme::alpha(crate::theme::MUTED, 0.35))
        .text_size(px(11.))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(crate::theme::alpha(crate::theme::MUTED_FOREGROUND, 1.0))
        .child(div().flex_1().child(i18n::t(locale, "governance.col.id")))
        .child(div().w(px(80.)).flex_shrink_0().child(i18n::t(locale, "governance.col.type")))
        .child(div().w(px(90.)).flex_shrink_0().child(i18n::t(locale, "governance.col.scope")))
        .child(div().flex_1().child(i18n::t(locale, "governance.col.detail")))
        .child(div().w(px(40.)).flex_shrink_0().child(""))
}

fn policy_row(row: &GovPolicyRow, is_last: bool) -> Div {
    let mut r = div()
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py_2p5()
        .text_size(px(crate::theme::TEXT_SM))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .font_family("SF Mono")
                .text_size(px(12.))
                .text_color(crate::theme::alpha(crate::theme::MUTED_FOREGROUND, 1.0))
                .child(SharedString::from(row.policy_id.clone())),
        )
        .child(
            div().w(px(80.)).flex_shrink_0().child(
                crate::mds_gpui::badge(SharedString::from(row.policy_type.clone()), type_badge_variant(&row.policy_type)),
            ),
        )
        .child(
            div()
                .w(px(90.))
                .flex_shrink_0()
                .font_family("SF Mono")
                .text_size(px(12.))
                .text_color(crate::theme::alpha(crate::theme::MUTED_FOREGROUND, 1.0))
                .child(scope_label(row)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(crate::theme::alpha(crate::theme::MUTED_FOREGROUND, 1.0))
                .child(SharedString::from(row.detail.clone())),
        )
        .child(div().w(px(40.)).flex_shrink_0().child(static_toggle(true)));
    if !is_last {
        r = r.border_b_1().border_color(crate::theme::border());
    }
    r
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch(state, cx);

    let locale = state.locale;
    let policies = cx.global::<GovernanceState>().policies.clone();
    let filter = cx.global::<GovernanceState>().filter;

    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .text_size(px(crate::theme::TEXT_XL))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(crate::theme::alpha(crate::theme::FOREGROUND, 1.0))
                        .child(i18n::t(locale, "governance.title")),
                )
                .child(
                    div()
                        .text_size(px(crate::theme::TEXT_SM))
                        .text_color(crate::theme::alpha(crate::theme::MUTED_FOREGROUND, 1.0))
                        .child(i18n::t(locale, "governance.subtitle")),
                ),
        )
        .child(crate::mds_gpui::button(
            "gov-add-rule",
            i18n::t(locale, "governance.addRule"),
            crate::mds_gpui::ButtonVariant::Primary,
            true, // decision-type action — assembled, not wired; see module header §
            None,
            |_ev, _window, _cx| {},
        ));

    let body: Div = match &policies {
        Loadable::Loading => div().flex().flex_col().gap_2().child(crate::mds_gpui::skeleton(px(760.), px(44.))).child(crate::mds_gpui::skeleton(px(760.), px(44.))),
        Loadable::Failed(err) => div().child(crate::screens::dashboard::error_row(locale, err)),
        Loadable::Ready(rows) => {
            let visible: Vec<&GovPolicyRow> = match filter {
                None => rows.iter().collect(),
                Some(t) => rows.iter().filter(|r| r.policy_type == t).collect(),
            };
            if visible.is_empty() {
                div().child(crate::mds_gpui::empty_state("⚖️", i18n::t(locale, "governance.empty"), None, None::<Div>))
            } else {
                let n = visible.len();
                let mut group = div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .rounded(px(crate::theme::RADIUS_XL))
                    .overflow_hidden()
                    .bg(crate::theme::alpha(crate::theme::SURFACE, 1.0))
                    .border_1()
                    .border_color(crate::theme::surface_border())
                    .child(header_row(locale));
                for (i, row) in visible.iter().enumerate() {
                    group = group.child(policy_row(row, i + 1 == n));
                }
                group
            }
        }
    };

    // Filter chips need real counts — computed from `Loadable::Ready` only;
    // Loading/Failed states show them all at "0" via an empty slice (the
    // chip row still renders, just inert until data lands, same as `inbox.
    // rs`'s own filter-chip-before-load behavior).
    let counted: &[GovPolicyRow] = match &policies {
        Loadable::Ready(rows) => rows,
        _ => &[],
    };
    let mut chip_row = div().flex().flex_wrap().gap_1p5();
    chip_row = chip_row.child(filter_chip(locale, None, counted.len(), filter.is_none(), cx));
    for t in POLICY_TYPES {
        let count = counted.iter().filter(|r| r.policy_type == t).count();
        chip_row = chip_row.child(filter_chip(locale, Some(t), count, filter == Some(t), cx));
    }

    div()
        .id("governance-page")
        .size_full()
        .overflow_y_scroll()
        .child(
            div()
                .max_w(px(880.))
                .mx_auto()
                .flex()
                .flex_col()
                .gap_3p5()
                .p_2()
                .child(breadcrumb("gov-breadcrumb", locale, cx))
                .child(shell_tabs(locale, "governance", cx))
                .child(header)
                .child(chip_row)
                .child(body),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate_policy() -> Value {
        json!({
            "scope": "global", "policy_type": "rate", "policy_id": "rate-mcp-01", "agent_id": "*",
            "resource": "mcp_calls", "limit": 200, "window_seconds": 60, "action_on_violation": "reject",
        })
    }

    fn permission_policy() -> Value {
        json!({
            "scope": "finance-bot", "policy_type": "permission", "policy_id": "perm-finance", "agent_id": "finance-bot",
            "allowed_scopes": ["memory:read", "memory:write"], "denied_scopes": ["odoo:write", "odoo:execute", "odoo:delete", "odoo:admin", "odoo:export"],
            "requires_approval": [],
        })
    }

    #[test]
    fn parse_governance_list_reads_every_policy_type() {
        let v = json!({ "policies": [rate_policy(), permission_policy()] });
        let rows = parse_governance_list(&v);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].policy_type, "rate");
        assert_eq!(rows[0].detail, "mcp_calls ≤ 200/60s → reject");
        assert_eq!(rows[1].policy_type, "permission");
        assert_eq!(rows[1].detail, "+2 / -5");
    }

    #[test]
    fn parse_governance_list_missing_array_is_empty_not_panicking() {
        assert!(parse_governance_list(&json!({})).is_empty());
        assert!(parse_governance_list(&json!(null)).is_empty());
    }

    #[test]
    fn parse_governance_list_skips_entries_without_policy_type() {
        let v = json!({ "policies": [{ "policy_id": "x" }] });
        assert!(parse_governance_list(&v).is_empty());
    }

    #[test]
    fn quota_and_lifecycle_details_format_correctly() {
        let quota = json!({
            "policy_type": "quota", "policy_id": "quota-daily", "agent_id": "*",
            "daily_token_budget": 800_000, "max_concurrent_tasks": 4,
        });
        let lifecycle = json!({
            "policy_type": "lifecycle", "policy_id": "life-idle-72h", "agent_id": "ephemeral-*",
            "max_idle_hours": 72, "health_check_interval_seconds": 300,
        });
        let rows = parse_governance_list(&json!({ "policies": [quota, lifecycle] }));
        assert_eq!(rows[0].detail, "800000 tok/day · 4 tasks");
        assert_eq!(rows[1].detail, "idle 72h · hc 300s");
    }

    #[test]
    fn scope_label_shows_star_for_global_and_agent_id_otherwise() {
        let global_row = GovPolicyRow {
            scope: "global".into(),
            policy_type: "rate".into(),
            policy_id: "x".into(),
            agent_id: "*".into(),
            detail: String::new(),
        };
        let scoped_row = GovPolicyRow {
            scope: "finance-bot".into(),
            policy_type: "permission".into(),
            policy_id: "y".into(),
            agent_id: "finance-bot".into(),
            detail: String::new(),
        };
        assert_eq!(scope_label(&global_row).to_string(), "*");
        assert_eq!(scope_label(&scoped_row).to_string(), "finance-bot");
    }

    #[test]
    fn policy_types_constant_matches_backend_gov_policy_types() {
        // Trip-wire against a silent divergence from `GOV_POLICY_TYPES`
        // (`crates/duduclaw-gateway/src/handlers.rs`) — this crate has no
        // dependency on that crate, so the 4-token list is duplicated here
        // as plain data (same "duplicated, not invented" precedent
        // `catalog_common::CATEGORY_ORDER`'s own header comment documents).
        assert_eq!(POLICY_TYPES, ["rate", "permission", "quota", "lifecycle"]);
    }
}
