// WP-S5b3-G (S5b 第三波) — "分析報表" (`nav.rs` id `reports`, monitor area —
// already a nav item since S5b1's rebuild pointed its budget-card button at
// it, but the page itself was a stub until this pass). Visual authority:
// `commercial/design/duduclaw-s5-viz-pages/Reports.dc.html` (B9, 總覽→深潛)
// — KPI 磚 → 對話趨勢折線 → 成本比較長條 → 各員工快取效率表, in that order.
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, never guessed) ────────────────────────────────────────────
//   `analytics.summary {period}` (dispatch L6201, handler L20789, manager+)
//   → `{total_conversations, total_messages, auto_reply_rate, avg_response_ms,
//   p95_response_ms, zero_cost_ratio, estimated_savings_cents, period}`.
//   `period` fixed to `"month"` — this page has no period switcher (the
//   web `<Segmented>` control is out of this pass's brief).
//   `analytics.conversations {}` (dispatch L6205, handler L20915, manager+)
//   → `{daily: [{date, count, auto_count}]}`.
//   `analytics.cost_savings {}` (dispatch L6209, handler L20955, manager+) →
//   `{monthly: [{month, human_cost, agent_cost, savings}]}` (cents).
//   `cost.summary {hours}` (dispatch L6308, handler L24336, admin) →
//   `CostSummary` (`available`, then millicent totals + `avg_cache_efficiency`/
//   `cache_hit_rate`). `cost.agents {hours}` (dispatch L6312, handler
//   L24399, admin) → `{available, agents: [CostAgentRow]}`.
//
// ── Deliberate scope cuts vs. `web/src/pages/ReportPage.tsx` ─────────────
// No period switcher (fixed "month"/720h), no notification-effectiveness
// section (`notify.stats` — not named in this pass's brief), no recent-
// usage transaction table (`cost.recent` — the brief names "各員工快取效率
// 表", i.e. `cost.agents`, not the per-request log), no 200K price-cliff
// banner. The brief names exactly four sections and this page implements
// exactly those four.
//
// ── Canvas-drawn charts (per this task's brief: "圖表用 gpui canvas 原生
// 繪製") ───────────────────────────────────────────────────────────────
// `spike_t7.rs::render_edges_section` proved `gpui::canvas` +
// `PathBuilder`/`paint_quad` on this exact gpui build — this page is the
// first REAL (non-spike) consumer of that recipe. Two charts:
//   1. 對話趨勢: a 2-series polyline (對話數/自動回覆), NOT the web page's 3-
//      legend mockup — `analytics.conversations` only ever returns
//      `count`/`auto_count` per day, no per-day message-count field exists
//      anywhere in the RPC (confirmed by reading `handle_analytics_
//      conversations`), so a third "訊息數" series would be fabricated data.
//      Labeled honestly with only the two series the canvas can support.
//   2. 成本比較: a 3-bar chart (人力成本 baseline=100% / AI 員工成本 / 淨節省)
//      from the MOST RECENT row of `analytics.cost_savings`'s `monthly`
//      array (the canvas shows one period's comparison, not a monthly
//      trend — this page picks "latest" rather than inventing an
//      aggregation).

use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::{json, Value};

use crate::i18n::{self, Locale};
use crate::mds_gpui::{empty_state, skeleton, table};
use crate::rpc::CallError;
use crate::theme;
use crate::ws_status::{self, WsConnState};
use crate::RootView;

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Clone)]
pub enum Loadable<T> {
    Loading,
    Ready(T),
    Failed(String),
}

impl<T> From<Result<T, String>> for Loadable<T> {
    fn from(r: Result<T, String>) -> Self {
        match r {
            Ok(v) => Loadable::Ready(v),
            Err(e) => Loadable::Failed(e),
        }
    }
}

#[derive(Clone, Default)]
pub struct Summary {
    pub total_conversations: i64,
    pub total_messages: i64,
    pub auto_reply_rate: f64,
    pub avg_response_ms: i64,
    pub estimated_savings_cents: i64,
    pub zero_cost_ratio: f64,
}

#[derive(Clone)]
pub struct DailyRow {
    pub date: String,
    pub count: i64,
    pub auto_count: i64,
}

#[derive(Clone)]
pub struct CostRow {
    pub month: String,
    pub human_cost: i64,
    pub agent_cost: i64,
    pub savings: i64,
}

#[derive(Clone, Default)]
pub struct CostSummary {
    pub available: bool,
    pub cache_hit_rate: f64,
    pub avg_cache_efficiency: f64,
}

#[derive(Clone)]
pub struct CostAgentRow {
    pub agent_id: String,
    pub avg_cache_efficiency: f64,
    pub total_requests: i64,
    pub total_cost_millicents: i64,
}

pub struct ReportsState {
    requested: bool,
    pub summary: Loadable<Summary>,
    pub daily: Loadable<Vec<DailyRow>>,
    pub cost_savings: Loadable<Vec<CostRow>>,
    pub cost_summary: Loadable<CostSummary>,
    pub cost_agents: Loadable<Vec<CostAgentRow>>,
}

impl Default for ReportsState {
    fn default() -> Self {
        Self {
            requested: false,
            summary: Loadable::Loading,
            daily: Loadable::Loading,
            cost_savings: Loadable::Loading,
            cost_summary: Loadable::Loading,
            cost_agents: Loadable::Loading,
        }
    }
}

impl Global for ReportsState {}

fn describe_call_error(e: &CallError) -> String {
    match e {
        CallError::NotConnected => "尚未連線到伺服器".to_string(),
        CallError::Timeout => "請求逾時".to_string(),
        CallError::Disconnected => "連線已中斷".to_string(),
        CallError::Rejected(v) => v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()),
    }
}

fn spawn_call(
    cx: &mut Context<RootView>,
    session_tx: tokio::sync::mpsc::UnboundedSender<ws_status::Command>,
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

fn parse_summary(v: &Value) -> Summary {
    Summary {
        total_conversations: v.get("total_conversations").and_then(Value::as_i64).unwrap_or(0),
        total_messages: v.get("total_messages").and_then(Value::as_i64).unwrap_or(0),
        auto_reply_rate: v.get("auto_reply_rate").and_then(Value::as_f64).unwrap_or(0.0),
        avg_response_ms: v.get("avg_response_ms").and_then(Value::as_i64).unwrap_or(0),
        estimated_savings_cents: v.get("estimated_savings_cents").and_then(Value::as_i64).unwrap_or(0),
        zero_cost_ratio: v.get("zero_cost_ratio").and_then(Value::as_f64).unwrap_or(0.0),
    }
}

fn parse_daily(v: &Value) -> Vec<DailyRow> {
    v.get("daily")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|d| DailyRow {
            date: d.get("date").and_then(Value::as_str).unwrap_or("").to_string(),
            count: d.get("count").and_then(Value::as_i64).unwrap_or(0),
            auto_count: d.get("auto_count").and_then(Value::as_i64).unwrap_or(0),
        })
        .collect()
}

fn parse_cost_savings(v: &Value) -> Vec<CostRow> {
    v.get("monthly")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|m| CostRow {
            month: m.get("month").and_then(Value::as_str).unwrap_or("").to_string(),
            human_cost: m.get("human_cost").and_then(Value::as_i64).unwrap_or(0),
            agent_cost: m.get("agent_cost").and_then(Value::as_i64).unwrap_or(0),
            savings: m.get("savings").and_then(Value::as_i64).unwrap_or(0),
        })
        .collect()
}

fn parse_cost_summary(v: &Value) -> CostSummary {
    CostSummary {
        available: v.get("available").and_then(Value::as_bool).unwrap_or(false),
        cache_hit_rate: v.get("cache_hit_rate").and_then(Value::as_f64).unwrap_or(0.0),
        avg_cache_efficiency: v.get("avg_cache_efficiency").and_then(Value::as_f64).unwrap_or(0.0),
    }
}

fn parse_cost_agents(v: &Value) -> Vec<CostAgentRow> {
    if !v.get("available").and_then(Value::as_bool).unwrap_or(false) {
        return Vec::new();
    }
    v.get("agents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|a| CostAgentRow {
            agent_id: a.get("agent_id").and_then(Value::as_str).unwrap_or("").to_string(),
            avg_cache_efficiency: a.get("avg_cache_efficiency").and_then(Value::as_f64).unwrap_or(0.0),
            total_requests: a.get("total_requests").and_then(Value::as_i64).unwrap_or(0),
            total_cost_millicents: a.get("total_cost_millicents").and_then(Value::as_i64).unwrap_or(0),
        })
        .collect()
}

pub fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if state.active_page != "reports" || cx.default_global::<ReportsState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<ReportsState>().requested = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx.clone(), "analytics.summary", json!({"period": "month"}), |cx, result| {
        cx.default_global::<ReportsState>().summary = result.map(|v| parse_summary(&v)).into();
    });
    spawn_call(cx, tx.clone(), "analytics.conversations", json!({}), |cx, result| {
        cx.default_global::<ReportsState>().daily = result.map(|v| parse_daily(&v)).into();
    });
    spawn_call(cx, tx.clone(), "analytics.cost_savings", json!({}), |cx, result| {
        cx.default_global::<ReportsState>().cost_savings = result.map(|v| parse_cost_savings(&v)).into();
    });
    spawn_call(cx, tx.clone(), "cost.summary", json!({"hours": 720}), |cx, result| {
        cx.default_global::<ReportsState>().cost_summary = result.map(|v| parse_cost_summary(&v)).into();
    });
    spawn_call(cx, tx, "cost.agents", json!({"hours": 720}), |cx, result| {
        cx.default_global::<ReportsState>().cost_agents = result.map(|v| parse_cost_agents(&v)).into();
    });
}

// ── Presentation helpers ──────────────────────────────────────────────

fn section_card(title: SharedString, body: Div) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2p5()
        .p_4()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::surface_shadow())
        .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(title))
        .child(body)
}

fn kpi_brick(value: SharedString, label: SharedString, value_color: Option<u32>) -> Div {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .gap_0p5()
        .p_3()
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .child(
            div()
                .text_size(px(theme::TEXT_BASE))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme::alpha(value_color.unwrap_or(theme::FOREGROUND), 1.0))
                .child(value),
        )
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(label))
}

fn kpi_section(locale: Locale, s: &Summary) -> Div {
    let grid = div()
        .grid()
        .grid_cols(3)
        .gap_2()
        .child(kpi_brick(s.total_conversations.to_string().into(), i18n::t(locale, "reports.conversations"), None))
        .child(kpi_brick(s.total_messages.to_string().into(), i18n::t(locale, "reports.messages"), None))
        .child(kpi_brick(format!("{:.0}%", s.auto_reply_rate * 100.0).into(), i18n::t(locale, "reports.autoReplyRate"), None))
        .child(kpi_brick(format!("{}ms", s.avg_response_ms).into(), i18n::t(locale, "reports.avgResponse"), None))
        .child(kpi_brick(format!("NT${}", s.estimated_savings_cents / 100).into(), i18n::t(locale, "reports.savings"), Some(theme::SUCCESS)))
        .child(kpi_brick(format!("{:.0}%", s.zero_cost_ratio * 100.0).into(), i18n::t(locale, "reports.zeroCostRatio"), None));
    grid
}

// ── Canvas chart 1: 對話趨勢 (2-series polyline) ──────────────────────────

fn trend_chart(rows: &[DailyRow]) -> Div {
    const WIDTH: f32 = 640.0;
    const HEIGHT: f32 = 160.0;
    const PAD: f32 = 8.0;

    if rows.is_empty() {
        return div().h(px(HEIGHT)).flex().items_center().justify_center();
    }
    let max = rows.iter().map(|d| d.count.max(d.auto_count)).max().unwrap_or(1).max(1) as f32;
    let n = rows.len().max(1);

    let count_series: Vec<(f32, f32)> = rows
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let x = PAD + (i as f32 / (n.saturating_sub(1).max(1)) as f32) * (WIDTH - PAD * 2.0);
            let y = HEIGHT - PAD - (d.count as f32 / max) * (HEIGHT - PAD * 2.0);
            (x, y)
        })
        .collect();
    let auto_series: Vec<(f32, f32)> = rows
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let x = PAD + (i as f32 / (n.saturating_sub(1).max(1)) as f32) * (WIDTH - PAD * 2.0);
            let y = HEIGHT - PAD - (d.auto_count as f32 / max) * (HEIGHT - PAD * 2.0);
            (x, y)
        })
        .collect();

    fn paint_series(bounds: gpui::Bounds<gpui::Pixels>, window: &mut gpui::Window, pts: &[(f32, f32)], color: u32) {
        if pts.len() < 2 {
            return;
        }
        let mut builder = gpui::PathBuilder::stroke(px(2.0));
        builder.move_to(bounds.origin + gpui::point(px(pts[0].0), px(pts[0].1)));
        for p in &pts[1..] {
            builder.line_to(bounds.origin + gpui::point(px(p.0), px(p.1)));
        }
        if let Ok(path) = builder.build() {
            window.paint_path(path, theme::alpha(color, 0.9));
        }
        for p in pts {
            let r = px(2.5);
            let center = bounds.origin + gpui::point(px(p.0), px(p.1));
            let dot_bounds = gpui::Bounds::new(gpui::point(center.x - r, center.y - r), gpui::size(r * 2., r * 2.));
            window.paint_quad(gpui::quad(dot_bounds, r, theme::alpha(color, 1.0), px(0.), gpui::transparent_black(), gpui::BorderStyle::default()));
        }
    }

    div().w_full().h(px(HEIGHT)).child(
        gpui::canvas(
            move |_bounds, _window, _cx| {},
            move |bounds, _prepaint, window, _cx| {
                paint_series(bounds, window, &count_series, theme::CHART_1);
                paint_series(bounds, window, &auto_series, theme::CHART_2);
            },
        )
        .size_full(),
    )
}

fn legend_dot(color: u32, label: SharedString) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .child(div().size(px(8.)).rounded_full().bg(theme::alpha(color, 1.0)))
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(label))
}

/// `"2026-08-20"` → `"8/20"` — the canvas's own date-axis format
/// ("8/14"..."8/20"). Falls back to the raw string for anything not
/// `YYYY-MM-DD` shaped (pure ASCII either way, so a byte slice is safe).
fn short_date(date: &str) -> String {
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() == 3 {
        let month = parts[1].trim_start_matches('0');
        let day = parts[2].trim_start_matches('0');
        format!("{}/{}", if month.is_empty() { "0" } else { month }, if day.is_empty() { "0" } else { day })
    } else {
        date.to_string()
    }
}

fn date_axis_row(rows: &[DailyRow]) -> Div {
    let mut row = div().flex().justify_between();
    for d in rows {
        row = row.child(div().text_size(px(10.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.8)).child(short_date(&d.date)));
    }
    row
}

fn trend_section(locale: Locale, daily: &[DailyRow]) -> Div {
    if daily.is_empty() {
        return section_card(i18n::t(locale, "reports.trend"), empty_state("📈", i18n::t(locale, "common.noData"), None, None::<Div>));
    }
    // Canvas shows exactly 7 days ("8/14"..."8/20") — matched here rather
    // than an arbitrary longer window.
    let rows: Vec<DailyRow> = daily.iter().rev().take(7).rev().cloned().collect();
    let body = div()
        .flex()
        .flex_col()
        .gap_1()
        .child(trend_chart(&rows))
        .child(date_axis_row(&rows))
        .child(
            div()
                .pt_1()
                .flex()
                .gap_4()
                .child(legend_dot(theme::CHART_1, i18n::t(locale, "reports.conversations")))
                .child(legend_dot(theme::CHART_2, i18n::t(locale, "reports.autoReply"))),
        );
    section_card(i18n::t(locale, "reports.trend"), body)
}

// ── Canvas chart 2: 成本比較 (3-bar) ───────────────────────────────────────

fn cost_bar_chart(human_pct: f32, agent_pct: f32, savings_pct: f32) -> Div {
    const WIDTH: f32 = 240.0;
    const HEIGHT: f32 = 120.0;
    const BAR_W: f32 = 48.0;
    const GAP: f32 = 32.0;

    let bars = [(human_pct, theme::CHART_3), (agent_pct, theme::CHART_1), (savings_pct, theme::SUCCESS)];
    div().w(px(WIDTH)).h(px(HEIGHT)).child(
        gpui::canvas(
            move |_bounds, _window, _cx| {},
            move |bounds, _prepaint, window, _cx| {
                for (i, (pct, color)) in bars.iter().enumerate() {
                    let h = (pct.clamp(0.0, 100.0) / 100.0) * HEIGHT;
                    let x = i as f32 * (BAR_W + GAP);
                    let y = HEIGHT - h;
                    let bar_bounds = gpui::Bounds::new(bounds.origin + gpui::point(px(x), px(y)), gpui::size(px(BAR_W), px(h)));
                    window.paint_quad(gpui::quad(bar_bounds, px(3.0), theme::alpha(*color, 0.85), px(0.), gpui::transparent_black(), gpui::BorderStyle::default()));
                }
            },
        )
        .size_full(),
    )
}

fn cost_comparison_section(locale: Locale, costs: &[CostRow]) -> Div {
    let Some(latest) = costs.last() else {
        return section_card(i18n::t(locale, "reports.costComparison"), empty_state("💰", i18n::t(locale, "common.noData"), None, None::<Div>));
    };
    let human = latest.human_cost.max(0) as f32;
    let agent = latest.agent_cost.max(0) as f32;
    let savings = latest.savings.max(0) as f32;
    let base = human.max(1.0);
    let (human_pct, agent_pct, savings_pct) = (100.0, (agent / base) * 100.0, (savings / base) * 100.0);

    let labels = div()
        .flex()
        .gap_8()
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_0p5()
                .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::CHART_3, 1.0)).child(format!("{human_pct:.0}%")))
                .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "reports.humanCost"))),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_0p5()
                .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::CHART_1, 1.0)).child(format!("{agent_pct:.0}%")))
                .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "reports.agentCost"))),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_0p5()
                .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::SUCCESS, 1.0)).child(format!("{savings_pct:.0}%")))
                .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "reports.netSavings"))),
        );

    let body = div().flex().items_center().gap_6().child(cost_bar_chart(human_pct, agent_pct, savings_pct)).child(labels);
    section_card(i18n::t1(locale, "reports.costComparison.forMonth", "month", &latest.month), body)
}

// ── 各員工快取效率表 ────────────────────────────────────────────────────

fn cache_health_label(locale: Locale, eff: f64) -> SharedString {
    if eff >= 0.7 {
        i18n::t(locale, "reports.cache.health.healthy")
    } else if eff >= 0.3 {
        i18n::t(locale, "reports.cache.health.normal")
    } else {
        i18n::t(locale, "reports.cache.health.degraded")
    }
}

fn cache_efficiency_section(locale: Locale, summary: &CostSummary, agents: &[CostAgentRow]) -> Div {
    if !summary.available {
        return section_card(i18n::t(locale, "reports.cache.title"), empty_state("🗄️", i18n::t(locale, "reports.cache.empty.title"), Some(i18n::t(locale, "reports.cache.empty.desc")), None::<Div>));
    }
    let mut col = div().flex().flex_col().gap_3();
    col = col.child(
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "reports.cache.hitRate")))
            .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(format!("{:.0}%", summary.cache_hit_rate * 100.0))),
    );
    if agents.is_empty() {
        col = col.child(empty_state("🗄️", i18n::t(locale, "common.noData"), None, None::<Div>));
    } else {
        let headers: Vec<SharedString> = vec![
            i18n::t(locale, "reports.cache.col.agent"),
            i18n::t(locale, "reports.cache.col.eff"),
            i18n::t(locale, "reports.cache.col.requests"),
            i18n::t(locale, "reports.cache.col.cost"),
        ];
        let mut rows: Vec<Vec<SharedString>> = Vec::with_capacity(agents.len());
        for a in agents {
            rows.push(vec![
                a.agent_id.clone().into(),
                format!("{:.0}% ({})", a.avg_cache_efficiency * 100.0, cache_health_label(locale, a.avg_cache_efficiency)).into(),
                a.total_requests.to_string().into(),
                format!("NT${:.2}", a.total_cost_millicents as f64 / 100_000.0).into(),
            ]);
        }
        col = col.child(table(&headers, &rows));
    }
    // Canvas's own "成本與快取效率 / 平均效率 85%" title+subtitle pairing.
    let title_row = div()
        .flex()
        .items_center()
        .justify_between()
        .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "reports.cache.byAgent")))
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::t1(locale, "reports.cache.avgEff", "value", &format!("{:.0}", summary.avg_cache_efficiency * 100.0))),
        );
    div()
        .flex()
        .flex_col()
        .gap_2p5()
        .p_4()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::surface_shadow())
        .child(title_row)
        .child(col)
}

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    maybe_fetch(state, cx);
    let locale = state.locale;

    if state.ws_state != WsConnState::Authenticated {
        return div().id("reports-page").size_full().flex().items_center().justify_center().child(empty_state(
            "🔌",
            i18n::t(locale, "native.home.connError.title"),
            Some(i18n::t(locale, "native.home.connError.desc")),
            None::<Div>,
        ));
    }

    let g = cx.default_global::<ReportsState>();
    let summary = g.summary.clone();
    let daily = g.daily.clone();
    let cost_savings = g.cost_savings.clone();
    let cost_summary = g.cost_summary.clone();
    let cost_agents = g.cost_agents.clone();

    let header = div()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(div().text_size(px(theme::TEXT_XL)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "nav.reports")))
        .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "nav.reports.desc")));

    let mut col = div().id("reports-page").size_full().overflow_y_scroll().flex().flex_col().gap_4().p_6().child(header);

    col = col.child(match summary {
        Loadable::Loading => skeleton(px(700.), px(80.)),
        Loadable::Failed(msg) => div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(msg),
        Loadable::Ready(s) => kpi_section(locale, &s),
    });

    col = col.child(match &daily {
        Loadable::Loading => skeleton(px(700.), px(200.)),
        Loadable::Failed(msg) => section_card(
            i18n::t(locale, "reports.trend"),
            div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(msg.clone()),
        ),
        Loadable::Ready(rows) => trend_section(locale, rows),
    });

    col = col.child(match &cost_savings {
        Loadable::Loading => skeleton(px(700.), px(160.)),
        Loadable::Failed(msg) => section_card(
            i18n::t(locale, "reports.costComparison"),
            div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(msg.clone()),
        ),
        Loadable::Ready(rows) => cost_comparison_section(locale, rows),
    });

    col = col.child(match (&cost_summary, &cost_agents) {
        (Loadable::Loading, _) | (_, Loadable::Loading) => skeleton(px(700.), px(200.)),
        (Loadable::Failed(msg), _) | (_, Loadable::Failed(msg)) => section_card(
            i18n::t(locale, "reports.cache.title"),
            div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(msg.clone()),
        ),
        (Loadable::Ready(s), Loadable::Ready(a)) => cache_efficiency_section(locale, s, a),
    });

    col
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_summary_reads_all_kpi_fields() {
        let v = json!({
            "total_conversations": 1842, "total_messages": 9406, "auto_reply_rate": 0.84,
            "avg_response_ms": 6200, "p95_response_ms": 9000, "zero_cost_ratio": 0.37,
            "estimated_savings_cents": 2_140_000, "period": "month",
        });
        let s = parse_summary(&v);
        assert_eq!(s.total_conversations, 1842);
        assert_eq!(s.estimated_savings_cents, 2_140_000);
    }

    #[test]
    fn parse_daily_and_cost_savings_read_arrays() {
        let daily = parse_daily(&json!({ "daily": [{"date": "2026-08-14", "count": 10, "auto_count": 8}] }));
        assert_eq!(daily.len(), 1);
        assert_eq!(daily[0].auto_count, 8);

        let costs = parse_cost_savings(&json!({ "monthly": [{"month": "2026-08", "human_cost": 10000, "agent_cost": 2400, "savings": 7600}] }));
        assert_eq!(costs.len(), 1);
        assert_eq!(costs[0].savings, 7600);
    }

    #[test]
    fn parse_cost_agents_empty_when_unavailable() {
        let agents = parse_cost_agents(&json!({ "available": false, "agents": [{"agent_id": "x"}] }));
        assert!(agents.is_empty());
    }

    #[test]
    fn parse_cost_agents_reads_rows_when_available() {
        let v = json!({ "available": true, "agents": [
            { "agent_id": "xiaodu", "cache_health": "healthy", "avg_cache_efficiency": 0.92,
              "total_requests": 1204, "total_cost_millicents": 186_000 },
        ]});
        let agents = parse_cost_agents(&v);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].agent_id, "xiaodu");
    }

    #[test]
    fn cache_health_label_thresholds() {
        assert_eq!(cache_health_label(Locale::En, 0.9), i18n::t(Locale::En, "reports.cache.health.healthy"));
        assert_eq!(cache_health_label(Locale::En, 0.5), i18n::t(Locale::En, "reports.cache.health.normal"));
        assert_eq!(cache_health_label(Locale::En, 0.1), i18n::t(Locale::En, "reports.cache.health.degraded"));
    }
}
