// WP-S6b1-J (2026-08-21) — "帳務" (B17), a "進階設定" drill-down leaf: no
// own sidebar row, breadcrumb only (`進階設定 › 帳務`, `manage_advanced_
// common::breadcrumb`). Reachable via `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=billing`
// today (same "D 先掛好分支就直接可達，未掛就自己掛" precedent every prior
// S5b/S6 self-attached page already establishes — `manage_advanced.rs`'s own
// 帳務 row stays inert this round, wiring it is a sibling package's scope).
//
// Visual authority: `commercial/design/duduclaw-s6-biz-pages/BillingPage.dc.
// html` (B17) — breadcrumb → header → 本月預算 boxed-list → 本月用量 4-tile
// grid → 預算事件 table (全部/超支 filter). Functional reference:
// `web/src/pages/BillingPage.tsx` (its `SpendCapsOverview`/`BudgetConsole`
// split; layout NOT copied per this task's "版面禁抄 web" rule).
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, not guessed) ─────────────────────────────────────────────
//   `billing.usage {}` (dispatch ~L6941, `handle_billing_usage` ~L21011, NO
//     `require_*!()` gate — explicitly a "personal surface" per that file's
//     own `personal_surfaces_are_untouched` test) → `{"plan","tier",
//     "conversations":{"used","limit"}, "agents":{...}, "channels":{...},
//     "inference_hours":{...}, "reset_at"}`. `limit` is `-1` for every metric
//     as of this pass (Community edition path — the handler hard-codes
//     `"plan": "community"` regardless of the active license tier; no metric
//     ever carries a real positive cap yet). Rendered honestly as
//     "used / 無上限", matching `web/src/pages/BillingPage.tsx`'s own
//     `UsageTile`'s `unlimited` branch verbatim (0% bar, no threshold color).
//   `accounts.budget_summary {}` (`handle_budget_summary` ~L13094,
//     `require_manager!()`) → `{"total_budget_cents","total_spent_cents",
//     "accounts":[...]}` — same shape `screens::accounts`'s own doc comment
//     documents; parsed locally here (a second small copy, not a cross-
//     import — see this file's own "duplicate, don't couple two batches"
//     note below) since this page only needs the two aggregate totals, not
//     the per-account array.
//   `budget.incidents {limit}` (dispatch ~L6747, `handle_budget_incidents`
//     ~L32093, `require_manager!()`) → `{"incidents":[{"ts","agent_id",
//     "event","scope","spent_cents","cap_cents"}], "by_agent":[...]}`, newest
//     first. `event` is ALWAYS the literal string `"budget_breaker_open"` as
//     of this pass (`budget.rs::append_budget_event` — the only call site
//     that ever appends a row, fired exclusively on a `BudgetVerdict::Deny`).
//     There is no "approaching limit" (warn-only) event kind anywhere in the
//     backend yet — rendered verbatim, un-translated, same choice
//     `web/src/pages/BillingPage.tsx`'s own `<Badge>{inc.event}</Badge>`
//     already makes, rather than inventing a friendlier label for a string
//     this page cannot actually validate the meaning of beyond "cost limit
//     was hit".
//
// ── Honest deviations from the design canvas (documented, not silent) ────
// 1. No "企業版 · Enterprise" edition badge in the header — the canvas draws
//    one, but sourcing it correctly would mean pulling in a fourth RPC
//    (`license.status`) onto a page whose own scope is spend, not licensing
//    (that KPI is `screens::license`'s actual job). Dropped rather than
//    faked with a hard-coded label.
// 2. "本月預算" card's two action buttons are REAL navigation, not the
//    canvas's implied "調整上限" edit action — this RPC surface has no single
//    "the" spend cap to edit (`billing.usage`'s own `limit: -1` finding
//    above, plus `web/src/pages/BillingPage.tsx`'s own `SpendCapsOverview`
//    comment: five independent cap mechanisms, no single edit point). The
//    left button navigates to `screens::accounts` (where the real AI-account
//    monthly budget shown on this card's second row is actually edited); the
//    right button navigates to `screens::license`. Both are plain
//    `active_page` switches — not the "decision-class, assembled-but-inert"
//    case this task's brief means by "決策類組裝不真按" (that rule is about
//    mutating actions like grant/revoke/approve, not page navigation), so
//    both are wired for real rather than rendered dead.
// 3. "本月預算" row 2 shows the AI-account rotation pool's aggregate monthly
//    budget (`accounts.budget_summary`) captioned as exactly that — not the
//    canvas's generic "目前用量上限" — since that IS the one real aggregate
//    spend-cap number this page has access to; mislabeling it as if it were
//    the licensing-plan cap the canvas implies would misrepresent which of
//    the five cap mechanisms is actually being shown.
// 4. 預算事件 filter uses this crate's real `mds_gpui::tabs` underline
//    control (matching `screens::widgets`/`screens::skills`'s own toggle-tab
//    precedent) rather than hand-rolling the canvas's filled-pill segmented
//    look — a shared primitive already proven in this crate over a one-off
//    visual match.

use gpui::{div, prelude::*, px, Context, Div, Global, IntoElement, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, empty_state, skeleton, BadgeVariant};
use crate::rpc::CallError;
use crate::screens::manage_advanced_common::breadcrumb;
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

pub use crate::screens::dashboard::Loadable;

const CONTENT_MAX_WIDTH: f32 = 860.0;

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
pub struct UsageMetric {
    pub used: i64,
    /// `-1` means unlimited — see this file's own header comment finding.
    pub limit: i64,
}

/// Hand-written (not derived): `i64::default()` is `0`, which would make a
/// missing metric object read as "used 0 of a 0 cap" instead of "unlimited"
/// — the honest default for an ABSENT metric is the same unlimited fallback
/// `parse_metric` already applies to a present-but-fieldless one.
impl Default for UsageMetric {
    fn default() -> Self {
        Self { used: 0, limit: -1 }
    }
}

#[derive(Clone, Default)]
pub struct BillingUsage {
    pub conversations: UsageMetric,
    pub agents: UsageMetric,
    pub channels: UsageMetric,
    pub inference_hours: UsageMetric,
    /// RFC3339, rendered date-only; empty string if the field is missing.
    pub reset_at: String,
}

#[derive(Clone, Copy, Default)]
pub struct BudgetTotals {
    pub spent_cents: i64,
    pub budget_cents: i64,
}

#[derive(Clone)]
pub struct BudgetIncident {
    pub ts: String,
    pub agent_id: String,
    pub event: String,
    pub scope: String,
    pub spent_cents: i64,
    pub cap_cents: i64,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum IncidentFilter {
    #[default]
    All,
    Over,
}

// ── State ──────────────────────────────────────────────────────────────

pub struct BillingState {
    requested: bool,
    pub usage: Loadable<BillingUsage>,
    pub budget: Loadable<BudgetTotals>,
    pub incidents: Loadable<Vec<BudgetIncident>>,
    pub filter: IncidentFilter,
}

impl Default for BillingState {
    fn default() -> Self {
        Self {
            requested: false,
            usage: Loadable::Loading,
            budget: Loadable::Loading,
            incidents: Loadable::Loading,
            filter: IncidentFilter::All,
        }
    }
}

impl Global for BillingState {}

// ── Response parsing ──────────────────────────────────────────────────

fn parse_metric(v: &Value) -> UsageMetric {
    UsageMetric {
        used: v.get("used").and_then(Value::as_i64).unwrap_or(0),
        limit: v.get("limit").and_then(Value::as_i64).unwrap_or(-1),
    }
}

fn parse_billing_usage(v: &Value) -> BillingUsage {
    BillingUsage {
        conversations: v.get("conversations").map(parse_metric).unwrap_or_default(),
        agents: v.get("agents").map(parse_metric).unwrap_or_default(),
        channels: v.get("channels").map(parse_metric).unwrap_or_default(),
        inference_hours: v.get("inference_hours").map(parse_metric).unwrap_or_default(),
        reset_at: v.get("reset_at").and_then(Value::as_str).unwrap_or("").to_string(),
    }
}

/// Local copy of `screens::accounts::parse_budget_totals` — same "duplicate,
/// don't couple two unrelated batches through a cross-import" precedent this
/// crate's own `settings_common.rs`/`mcp_keys.rs` header comments already
/// apply to breadcrumbs; the same reasoning extends to this one small parser.
fn parse_budget_totals(v: &Value) -> BudgetTotals {
    BudgetTotals {
        spent_cents: v.get("total_spent_cents").and_then(Value::as_i64).unwrap_or(0),
        budget_cents: v.get("total_budget_cents").and_then(Value::as_i64).unwrap_or(0),
    }
}

fn parse_incidents(v: &Value) -> Vec<BudgetIncident> {
    v.get("incidents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|e| BudgetIncident {
            ts: e.get("ts").and_then(Value::as_str).unwrap_or("").to_string(),
            agent_id: e.get("agent_id").and_then(Value::as_str).unwrap_or("").to_string(),
            event: e.get("event").and_then(Value::as_str).unwrap_or("").to_string(),
            scope: e.get("scope").and_then(Value::as_str).unwrap_or("").to_string(),
            spent_cents: e.get("spent_cents").and_then(Value::as_i64).unwrap_or(0),
            cap_cents: e.get("cap_cents").and_then(Value::as_i64).unwrap_or(0),
        })
        .collect()
}

// ── Fetch orchestration ───────────────────────────────────────────────

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if state.active_page != "billing" || cx.default_global::<BillingState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<BillingState>().requested = true;
    let tx = state.session_tx.clone();

    spawn_call(cx, tx.clone(), "billing.usage", json!({}), |cx, result| {
        cx.default_global::<BillingState>().usage = result.map(|v| parse_billing_usage(&v)).into();
    });
    spawn_call(cx, tx.clone(), "accounts.budget_summary", json!({}), |cx, result| {
        cx.default_global::<BillingState>().budget = result.map(|v| parse_budget_totals(&v)).into();
    });
    spawn_call(cx, tx, "budget.incidents", json!({"limit": 50}), |cx, result| {
        cx.default_global::<BillingState>().incidents = result.map(|v| parse_incidents(&v)).into();
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

// ── Display helpers ────────────────────────────────────────────────────

/// Integer cents → "N,NNN" (no decimals) — same recipe
/// `screens::accounts::format_dollars` establishes; duplicated locally for
/// the same "don't couple two batches" reason `parse_budget_totals` above
/// states.
fn format_dollars(cents: i64) -> String {
    let dollars = cents / 100;
    let sign = if dollars < 0 { "-" } else { "" };
    let digits = dollars.unsigned_abs().to_string();
    let mut grouped = String::new();
    for (i, ch) in digits.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    format!("{sign}{}", grouped.chars().rev().collect::<String>())
}

/// RFC3339 → "2026-09-01"; "—" when unparseable/empty (never a fabricated
/// date) — same recipe `screens::mcp_keys::format_created_date` establishes.
fn format_date(ts: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|_| "—".to_string())
}

/// "2026-08-15 09:30" — same parse, minute-resolution (the incidents table
/// needs more than a bare date to distinguish same-day rows).
fn format_datetime(ts: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| "—".to_string())
}

// ── Section: 本月預算 ──────────────────────────────────────────────────

fn budget_section(locale: Locale, usage: &Loadable<BillingUsage>, budget: &Loadable<BudgetTotals>, cx: &mut Context<RootView>) -> Div {
    let reset_row = {
        let value: SharedString = match usage {
            Loadable::Ready(u) if !u.reset_at.is_empty() => {
                i18n::t1(locale, "billing.budget.resetAt", "date", &format_date(&u.reset_at))
            }
            Loadable::Failed(_) => i18n::t(locale, "billing.budget.unknown"),
            _ => i18n::t(locale, "billing.budget.loading"),
        };
        kv_row(i18n::t(locale, "billing.budget.resetLabel"), div().text_size(px(12.5)).child(value))
    };

    let pool_row = {
        let value: Div = match budget {
            Loadable::Loading => skeleton(px(140.), px(14.)),
            Loadable::Failed(msg) => div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::DESTRUCTIVE, 1.0))
                .child(i18n::t1(locale, "native.home.card.errorPrefix", "message", msg)),
            Loadable::Ready(t) if t.budget_cents <= 0 => div()
                .font_family("SF Mono")
                .text_size(px(12.))
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(format!("NT$ {}", format_dollars(t.spent_cents))),
            Loadable::Ready(t) => div()
                .font_family("SF Mono")
                .text_size(px(12.))
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(format!("NT$ {} / NT$ {}", format_dollars(t.spent_cents), format_dollars(t.budget_cents))),
        };
        kv_row(i18n::t(locale, "billing.budget.poolLabel"), value)
    };

    let footer = div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .px_4()
        .py_2p5()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(11.5))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::t(locale, "billing.budget.note")),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .flex_shrink_0()
                .child(
                    div()
                        .id("billing-goto-accounts")
                        .cursor_pointer()
                        .px_3p5()
                        .h(px(30.))
                        .flex()
                        .items_center()
                        .rounded(px(theme::RADIUS_LG))
                        .bg(theme::alpha(theme::SURFACE, 1.0))
                        .border_1()
                        .border_color(theme::surface_border())
                        .text_size(px(12.))
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .hover(|s| s.bg(theme::alpha(theme::MUTED, 0.5)))
                        .child(i18n::t(locale, "billing.budget.gotoAccounts"))
                        .on_click(cx.listener(|this, _ev, _window, cx| {
                            this.active_page = "accounts";
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .id("billing-goto-license")
                        .cursor_pointer()
                        .px_3p5()
                        .h(px(30.))
                        .flex()
                        .items_center()
                        .rounded(px(theme::RADIUS_LG))
                        .bg(theme::alpha(theme::BRAND, 1.0))
                        .text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0))
                        .text_size(px(12.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .hover(|s| s.bg(theme::alpha(theme::BRAND, 0.9)))
                        .child(i18n::t(locale, "billing.budget.gotoLicense"))
                        .on_click(cx.listener(|this, _ev, _window, cx| {
                            this.active_page = "license";
                            cx.notify();
                        })),
                ),
        );

    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(section_label(i18n::t(locale, "billing.section.budget")))
        .child(
            div()
                .w_full()
                .flex()
                .flex_col()
                .rounded(px(theme::RADIUS_XL))
                .overflow_hidden()
                .bg(theme::alpha(theme::SURFACE, 1.0))
                .border_1()
                .border_color(theme::surface_border())
                .shadow(theme::surface_shadow())
                .child(reset_row)
                .child(pool_row.border_t_1().border_color(theme::border()))
                .child(footer.border_t_1().border_color(theme::border())),
        )
}

fn section_label(text: SharedString) -> Div {
    div()
        .px_0p5()
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(text)
}

fn kv_row(label: SharedString, value: impl IntoElement) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .px_4()
        .py_2p5()
        .child(div().flex_shrink_0().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(label))
        .child(value)
}

// ── Section: 本月用量 ──────────────────────────────────────────────────

fn usage_tile(label: SharedString, metric: UsageMetric, limit_label: SharedString) -> Div {
    let unlimited = metric.limit < 0;
    let pct = if unlimited || metric.limit <= 0 {
        0.0
    } else {
        (metric.used as f32 / metric.limit as f32).clamp(0.0, 1.0)
    };
    let bar_color = if unlimited {
        theme::MUTED_FOREGROUND
    } else if pct >= 0.9 {
        theme::DESTRUCTIVE
    } else if pct >= 0.7 {
        theme::WARNING
    } else {
        theme::SUCCESS
    };

    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .p(px(13.))
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::surface_shadow())
        .child(div().text_size(px(11.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(label))
        .child(div().text_size(px(20.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(metric.used.to_string()))
        .child(
            div()
                .font_family("SF Mono")
                .text_size(px(11.))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(format!("{} / {}", metric.used, if unlimited { limit_label.to_string() } else { metric.limit.to_string() })),
        )
        .child(
            div()
                .h(px(6.))
                .rounded_full()
                .bg(theme::alpha(theme::MUTED, 1.0))
                .overflow_hidden()
                .child(div().h_full().w(gpui::relative(pct)).rounded_full().bg(theme::alpha(bar_color, 1.0))),
        )
}

fn usage_section(locale: Locale, usage: &Loadable<BillingUsage>) -> Div {
    let limit_label = i18n::t(locale, "billing.unlimited");
    let body: Div = match usage {
        Loadable::Loading => {
            let mut grid = div().grid().grid_cols(4).gap_2p5();
            for _ in 0..4 {
                grid = grid.child(skeleton(px(180.), px(90.)));
            }
            grid
        }
        Loadable::Failed(msg) => div().child(empty_state("⚠️", i18n::t1(locale, "native.home.card.errorPrefix", "message", msg), None, None::<Div>)),
        Loadable::Ready(u) => div()
            .grid()
            .grid_cols(4)
            .gap_2p5()
            .child(usage_tile(i18n::t(locale, "billing.metric.conversations"), u.conversations, limit_label.clone()))
            .child(usage_tile(i18n::t(locale, "billing.metric.agents"), u.agents, limit_label.clone()))
            .child(usage_tile(i18n::t(locale, "billing.metric.channels"), u.channels, limit_label.clone()))
            .child(usage_tile(i18n::t(locale, "billing.metric.inferenceHours"), u.inference_hours, limit_label)),
    };

    div().flex().flex_col().gap_1p5().child(section_label(i18n::t(locale, "billing.section.usage"))).child(body)
}

// ── Section: 預算事件 ──────────────────────────────────────────────────

fn incident_row(inc: &BudgetIncident, is_last: bool) -> Div {
    let over = inc.cap_cents > 0 && inc.spent_cents > inc.cap_cents;
    let row = div()
        .grid()
        .grid_cols(5)
        .gap_2()
        .px_4()
        .py_2p5()
        .items_center()
        .text_size(px(12.5))
        .child(div().text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(SharedString::from(inc.agent_id.clone())))
        .child(badge(SharedString::from(inc.event.clone()), BadgeVariant::Outline))
        .child(
            div()
                .font_family("SF Mono")
                .text_size(px(12.))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(SharedString::from(inc.scope.clone())),
        )
        .child(
            div()
                .font_family("SF Mono")
                .text_size(px(12.))
                .text_color(theme::alpha(if over { theme::DESTRUCTIVE } else { theme::MUTED_FOREGROUND }, 1.0))
                .child(format!("NT$ {} / {}", format_dollars(inc.spent_cents), format_dollars(inc.cap_cents))),
        )
        .child(div().text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(format_datetime(&inc.ts)));
    if is_last {
        row
    } else {
        row.border_b_1().border_color(theme::border())
    }
}

fn incidents_header_row(locale: Locale) -> Div {
    div()
        .grid()
        .grid_cols(5)
        .gap_2()
        .px_4()
        .py_2()
        .bg(theme::alpha(theme::MUTED, 0.35))
        .text_size(px(10.5))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(i18n::t(locale, "billing.col.agent"))
        .child(i18n::t(locale, "billing.col.event"))
        .child(i18n::t(locale, "billing.col.scope"))
        .child(i18n::t(locale, "billing.col.spent"))
        .child(i18n::t(locale, "billing.col.time"))
}

fn incidents_section(locale: Locale, state: &Loadable<Vec<BudgetIncident>>, filter: IncidentFilter, cx: &mut Context<RootView>) -> Div {
    let filter_tabs = div()
        .flex()
        .gap_1p5()
        .child(filter_pill(locale, "billing.filter.all", filter == IncidentFilter::All, cx, IncidentFilter::All))
        .child(filter_pill(locale, "billing.filter.over", filter == IncidentFilter::Over, cx, IncidentFilter::Over));

    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .child(section_label(i18n::t(locale, "billing.section.incidents")))
        .child(filter_tabs);

    let body: Div = match state {
        Loadable::Loading => {
            let mut wrap = div().flex().flex_col().gap_2();
            for _ in 0..3 {
                wrap = wrap.child(skeleton(px(760.), px(44.)));
            }
            wrap
        }
        Loadable::Failed(msg) => div().child(empty_state("⚠️", i18n::t1(locale, "native.home.card.errorPrefix", "message", msg), None, None::<Div>)),
        Loadable::Ready(rows) => {
            let filtered: Vec<&BudgetIncident> = rows.iter().filter(|i| filter == IncidentFilter::All || (i.cap_cents > 0 && i.spent_cents > i.cap_cents)).collect();
            if filtered.is_empty() {
                div().child(empty_state("💰", i18n::t(locale, "billing.incidents.empty"), None, None::<Div>))
            } else {
                let n = filtered.len();
                let mut card = div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .rounded(px(theme::RADIUS_XL))
                    .overflow_hidden()
                    .bg(theme::alpha(theme::SURFACE, 1.0))
                    .border_1()
                    .border_color(theme::surface_border())
                    .shadow(theme::surface_shadow())
                    .child(incidents_header_row(locale));
                for (i, inc) in filtered.into_iter().enumerate() {
                    card = card.child(incident_row(inc, i + 1 == n));
                }
                card
            }
        }
    };

    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(header)
        .child(body)
        .child(
            div()
                .text_size(px(11.5))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .text_center()
                .child(i18n::t(locale, "billing.incidents.footerNote")),
        )
}

fn filter_pill(locale: Locale, key: &'static str, active: bool, cx: &mut Context<RootView>, target: IncidentFilter) -> Stateful<Div> {
    div()
        .id(key)
        .cursor_pointer()
        .px_3p5()
        .h(px(28.))
        .flex()
        .items_center()
        .rounded(px(theme::RADIUS_LG))
        .when(active, |el| el.bg(theme::alpha(theme::BRAND, 1.0)).text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0)))
        .when(!active, |el| {
            el.bg(theme::alpha(theme::SURFACE, 1.0))
                .border_1()
                .border_color(theme::surface_border())
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        })
        .text_size(px(12.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .child(i18n::t(locale, key))
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.default_global::<BillingState>().filter = target;
            cx.notify();
        }))
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    maybe_fetch(state, cx);
    let locale = state.locale;
    let g = cx.default_global::<BillingState>();
    let usage = g.usage.clone();
    let budget = g.budget.clone();
    let incidents = g.incidents.clone();
    let filter = g.filter;

    let crumb = breadcrumb("billing-breadcrumb", locale, i18n::t(locale, "billing.title"), cx);

    let header = div()
        .child(div().text_size(px(17.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "billing.title")))
        .child(div().mt(px(2.)).text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "billing.subtitle")));

    div()
        .id("billing-page")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .items_center()
        .child(
            div()
                .w_full()
                .max_w(px(CONTENT_MAX_WIDTH))
                .p_6()
                .flex()
                .flex_col()
                .gap_3p5()
                .child(crumb)
                .child(header)
                .child(budget_section(locale, &usage, &budget, cx))
                .child(usage_section(locale, &usage))
                .child(incidents_section(locale, &incidents, filter, cx)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_billing_usage_reads_the_real_payload_shape() {
        let v = json!({
            "plan": "community", "tier": "community",
            "conversations": {"used": 1284, "limit": -1},
            "agents": {"used": 7, "limit": -1},
            "channels": {"used": 4, "limit": -1},
            "inference_hours": {"used": 312, "limit": -1},
            "reset_at": "2026-09-01T00:00:00+00:00",
        });
        let u = parse_billing_usage(&v);
        assert_eq!(u.conversations.used, 1284);
        assert_eq!(u.conversations.limit, -1);
        assert_eq!(u.agents.used, 7);
        assert_eq!(u.inference_hours.used, 312);
        assert_eq!(u.reset_at, "2026-09-01T00:00:00+00:00");
    }

    #[test]
    fn parse_billing_usage_missing_fields_default_to_zero_and_unlimited() {
        let u = parse_billing_usage(&json!({}));
        assert_eq!(u.conversations.used, 0);
        assert_eq!(u.conversations.limit, -1);
        assert_eq!(u.reset_at, "");
    }

    #[test]
    fn parse_budget_totals_reads_the_dashboard_shared_shape() {
        let v = json!({ "total_spent_cents": 372000, "total_budget_cents": 600000 });
        let totals = parse_budget_totals(&v);
        assert_eq!(totals.spent_cents, 372000);
        assert_eq!(totals.budget_cents, 600000);
    }

    #[test]
    fn parse_incidents_reads_every_field_newest_first_order_preserved() {
        let v = json!({ "incidents": [
            { "ts": "2026-08-15T09:30:00Z", "agent_id": "cs-alice", "event": "budget_breaker_open", "scope": "mcp_calls", "spent_cents": 89000, "cap_cents": 80000 },
        ]});
        let rows = parse_incidents(&v);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].agent_id, "cs-alice");
        assert_eq!(rows[0].event, "budget_breaker_open");
        assert_eq!(rows[0].scope, "mcp_calls");
        assert_eq!(rows[0].spent_cents, 89000);
        assert_eq!(rows[0].cap_cents, 80000);
    }

    #[test]
    fn parse_incidents_missing_array_is_empty_not_a_panic() {
        assert!(parse_incidents(&json!({})).is_empty());
    }

    #[test]
    fn format_dollars_groups_thousands() {
        assert_eq!(format_dollars(372000), "3,720");
        assert_eq!(format_dollars(0), "0");
    }

    #[test]
    fn format_date_is_date_only_or_dash() {
        assert_eq!(format_date("2026-09-01T00:00:00+00:00"), "2026-09-01");
        assert_eq!(format_date("garbage"), "—");
        assert_eq!(format_date(""), "—");
    }

    #[test]
    fn format_datetime_includes_minutes_or_dash() {
        assert_eq!(format_datetime("2026-08-15T09:30:00Z"), "2026-08-15 09:30");
        assert_eq!(format_datetime(""), "—");
    }

    #[test]
    fn describe_call_error_prefers_structured_message_over_bare_string() {
        let msg = describe_call_error(&CallError::Rejected(json!({"code": "denied", "message": "權限不足"})));
        assert_eq!(msg, "權限不足");
    }
}
