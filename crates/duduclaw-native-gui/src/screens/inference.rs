// WP-S6b3-P (S6b 第三波, 2026-08-22) — "模型用量" (`Inference.dc.html`, B9
// Tabs 切模型供應商 + KPI 磚 + 單一大圖表). A "進階設定" drill-down leaf
// (`active_page == "inference"`, no `nav.rs` entry — wired from
// `manage_advanced.rs`'s 模型用量 row by this same pass; QA's own "drill-
// down, not a top-level nav item" ruling for this page).
//
// ── RPC shape (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, never guessed) ────────────────────────────────────────────
//   `cost.summary {hours}` (dispatch L6308, handler `handle_cost_summary`
//   L24336, admin) → `{available, total_requests, total_cost_millicents,
//   …}`. Called twice (`hours: 24` for 今日, `hours: 720` for 本月) — same
//   two-window shape `billing.rs`/`reports.rs` already establish for
//   "today vs. this month" KPI pairs.
//   `cost.recent {limit}` (dispatch L6316, handler `handle_cost_recent`
//   L24426, admin) → `{available, records: [{agent_id, request_type,
//   model, input_tokens, …, cost_millicents, created_at}]}`, newest first.
//   `model` is the ONLY field this RPC family carries that can distinguish
//   Claude/GPT/Gemini/local — see `provider_of` below for the honest
//   substring classifier this page derives from it (no separate "provider"
//   field exists).
//   `inference.get {}` (dispatch L5539, handler `handle_inference_get`
//   L10178) → the raw `inference.toml` passthrough (`inference_table_to_
//   response`, L2876-2902) — this page reads only the root `enabled`/
//   `backend`/`default_model` fields, on the 本地模型 tab only (see
//   deviation §3).
//
// ── Deliberate deviations from the canvas (documented, not silent) ────────
// 1. **Tabs are real, driven by one shared `cost.recent` fetch** — every
//    tab (Claude/GPT/Gemini/本地模型) filters the SAME already-fetched
//    record list by `provider_of(record.model)`, not 4 separate RPC calls.
// 2. **KPI tiles are fleet-wide, not per-tab.** `cost.summary` has no
//    per-provider breakdown (only `cost.agents`, per-AGENT not per-model) —
//    the 3 tiles (今日花費/本月花費/今日呼叫次數) show the same real totals
//    regardless of the active tab, rather than fabricating a per-provider
//    split this RPC family cannot produce.
// 3. **本地模型 tab additionally shows real `inference.get()` config**
//    (啟用狀態/後端/預設模型) — a second, DIFFERENT RPC family
//    (`inference.*`, not `cost.*`) this task's own brief explicitly names
//    ("cost.*/inference 類 RPC"). Cross-referencing it only on the one tab
//    it's actually relevant to (local inference config) avoids a spurious
//    call on the other three cloud-provider tabs.
// 4. **Chart window is "last `limit` records", not a strict 12-hour
//    window.** `cost.recent` has no time-range param, only `limit` (capped
//    500, L24427-24431) — under low traffic the fetched page may span more
//    or less than 12 real hours. Bucketed into the same 12 fixed hourly
//    slots the canvas draws; an honest approximation, not a guaranteed
//    12-hour boundary.

use gpui::{div, prelude::*, px, Bounds, Context, Div, Global, Pixels, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{empty_state, skeleton, tabs, TabItem};
use crate::rpc::CallError;
use crate::screens::manage_advanced_common::breadcrumb;
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

pub use crate::screens::dashboard::Loadable;

const PROVIDER_TABS: [&str; 4] = ["claude", "gpt", "gemini", "local"];
const RECENT_LIMIT: u64 = 500;

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Clone, Copy, Default)]
pub struct CostTotals {
    pub available: bool,
    pub total_requests: i64,
    pub total_cost_millicents: i64,
}

pub fn parse_cost_summary(v: &Value) -> CostTotals {
    CostTotals {
        available: v.get("available").and_then(Value::as_bool).unwrap_or(false),
        total_requests: v.get("total_requests").and_then(Value::as_i64).unwrap_or(0),
        total_cost_millicents: v.get("total_cost_millicents").and_then(Value::as_i64).unwrap_or(0),
    }
}

#[derive(Clone)]
pub struct CostRecord {
    pub model: String,
    pub created_at: String,
}

pub fn parse_cost_recent(v: &Value) -> Vec<CostRecord> {
    if !v.get("available").and_then(Value::as_bool).unwrap_or(false) {
        return Vec::new();
    }
    v.get("records")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|r| CostRecord {
            model: r.get("model").and_then(Value::as_str).unwrap_or("").to_string(),
            created_at: r.get("created_at").and_then(Value::as_str).unwrap_or("").to_string(),
        })
        .collect()
}

#[derive(Clone, Default)]
pub struct LocalInferenceInfo {
    pub enabled: bool,
    pub backend: String,
    pub default_model: String,
}

pub fn parse_inference_get(v: &Value) -> LocalInferenceInfo {
    LocalInferenceInfo {
        enabled: v.get("enabled").and_then(Value::as_bool).unwrap_or(false),
        backend: v.get("backend").and_then(Value::as_str).unwrap_or("").to_string(),
        default_model: v.get("default_model").and_then(Value::as_str).unwrap_or("").to_string(),
    }
}

/// The only classifier this RPC family can support — see module header §1.
/// Case-insensitive substring match on the real `model` field; anything not
/// matching a known cloud provider name is treated as local.
pub fn provider_of(model: &str) -> &'static str {
    let m = model.to_lowercase();
    if m.contains("claude") {
        "claude"
    } else if m.contains("gpt") {
        "gpt"
    } else if m.contains("gemini") {
        "gemini"
    } else {
        "local"
    }
}

// ── State ──────────────────────────────────────────────────────────────

pub struct InferenceUsageState {
    requested: bool,
    pub tab: &'static str,
    pub today: Loadable<CostTotals>,
    pub month: Loadable<CostTotals>,
    pub recent: Loadable<Vec<CostRecord>>,
    pub local_config: Loadable<LocalInferenceInfo>,
}

impl Default for InferenceUsageState {
    fn default() -> Self {
        Self {
            requested: false,
            tab: "claude",
            today: Loadable::Loading,
            month: Loadable::Loading,
            recent: Loadable::Loading,
            local_config: Loadable::Loading,
        }
    }
}

impl Global for InferenceUsageState {}

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if state.active_page != "inference" || cx.default_global::<InferenceUsageState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<InferenceUsageState>().requested = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx.clone(), "cost.summary", json!({"hours": 24}), |cx, result| {
        cx.default_global::<InferenceUsageState>().today = result.map(|v| parse_cost_summary(&v)).into();
    });
    spawn_call(cx, tx.clone(), "cost.summary", json!({"hours": 720}), |cx, result| {
        cx.default_global::<InferenceUsageState>().month = result.map(|v| parse_cost_summary(&v)).into();
    });
    spawn_call(cx, tx.clone(), "cost.recent", json!({"limit": RECENT_LIMIT}), |cx, result| {
        cx.default_global::<InferenceUsageState>().recent = result.map(|v| parse_cost_recent(&v)).into();
    });
    spawn_call(cx, tx, "inference.get", json!({}), |cx, result| {
        cx.default_global::<InferenceUsageState>().local_config = result.map(|v| parse_inference_get(&v)).into();
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

fn provider_label(locale: Locale, id: &str) -> SharedString {
    match id {
        "claude" => i18n::t(locale, "inference.tab.claude"),
        "gpt" => i18n::t(locale, "inference.tab.gpt"),
        "gemini" => i18n::t(locale, "inference.tab.gemini"),
        _ => i18n::t(locale, "inference.tab.local"),
    }
}

/// Millicents → "NT$N" (whole dollars) — same `/ 100_000.0` conversion
/// `reports.rs::cache_efficiency_section` already establishes.
fn format_ntd(millicents: i64) -> String {
    format!("NT${}", (millicents as f64 / 100_000.0).round() as i64)
}

// ── KPI tiles (fleet-wide — see module header §2) ─────────────────────

fn kpi_tile(value: SharedString, label: SharedString) -> Div {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .gap_1()
        .p(px(13.))
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .child(div().text_size(px(11.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(label))
        .child(div().text_size(px(19.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(value))
}

fn kpi_row(locale: Locale, today: &Loadable<CostTotals>, month: &Loadable<CostTotals>) -> Div {
    let today_cost: SharedString = match today {
        Loadable::Ready(t) if t.available => format_ntd(t.total_cost_millicents).into(),
        Loadable::Ready(_) => "—".into(),
        Loadable::Failed(_) => "—".into(),
        Loadable::Loading => "…".into(),
    };
    let today_calls: SharedString = match today {
        Loadable::Ready(t) if t.available => t.total_requests.to_string().into(),
        Loadable::Ready(_) => "—".into(),
        Loadable::Failed(_) => "—".into(),
        Loadable::Loading => "…".into(),
    };
    let month_cost: SharedString = match month {
        Loadable::Ready(t) if t.available => format_ntd(t.total_cost_millicents).into(),
        Loadable::Ready(_) => "—".into(),
        Loadable::Failed(_) => "—".into(),
        Loadable::Loading => "…".into(),
    };

    div()
        .flex()
        .gap_2p5()
        .child(kpi_tile(today_cost, i18n::t(locale, "inference.kpi.todayCost")))
        .child(kpi_tile(month_cost, i18n::t(locale, "inference.kpi.monthCost")))
        .child(kpi_tile(today_calls, i18n::t(locale, "inference.kpi.todayCalls")))
}

// ── Canvas-drawn chart: 呼叫量 · 近 12 小時 (same filled-line recipe
// `reliability.rs::incident_chart` establishes — duplicated locally per
// this pass's own "duplicate, don't couple across batches" convention). ──

fn hourly_call_buckets(records: &[&CostRecord], now: chrono::DateTime<chrono::Utc>) -> [u32; 12] {
    let mut buckets = [0u32; 12];
    for r in records {
        let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&r.created_at) else { continue };
        let dt = dt.with_timezone(&chrono::Utc);
        let hours_ago = (now - dt).num_hours();
        if (0..12).contains(&hours_ago) {
            buckets[11 - hours_ago as usize] += 1;
        }
    }
    buckets
}

fn call_volume_chart(buckets: [u32; 12]) -> Div {
    const WIDTH: f32 = 720.0;
    const HEIGHT: f32 = 150.0;
    const PAD: f32 = 6.0;

    let max = buckets.iter().copied().max().unwrap_or(0).max(1) as f32;
    let n = buckets.len();
    let points: Vec<(f32, f32)> = buckets
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            let x = PAD + (i as f32 / (n - 1) as f32) * (WIDTH - PAD * 2.0);
            let y = HEIGHT - PAD - (c as f32 / max) * (HEIGHT - PAD * 2.0);
            (x, y)
        })
        .collect();

    div().w_full().h(px(HEIGHT)).child(
        gpui::canvas(
            move |_bounds, _window, _cx| {},
            move |bounds, _prepaint, window, _cx| {
                if points.len() < 2 {
                    return;
                }
                let mut fill = gpui::PathBuilder::fill();
                fill.move_to(bounds.origin + gpui::point(px(points[0].0), px(HEIGHT)));
                for p in &points {
                    fill.line_to(bounds.origin + gpui::point(px(p.0), px(p.1)));
                }
                fill.line_to(bounds.origin + gpui::point(px(points[points.len() - 1].0), px(HEIGHT)));
                if let Ok(path) = fill.build() {
                    window.paint_path(path, theme::alpha(theme::BRAND, 0.08));
                }
                let mut stroke = gpui::PathBuilder::stroke(px(2.2));
                stroke.move_to(bounds.origin + gpui::point(px(points[0].0), px(points[0].1)));
                for p in &points[1..] {
                    stroke.line_to(bounds.origin + gpui::point(px(p.0), px(p.1)));
                }
                if let Ok(path) = stroke.build() {
                    window.paint_path(path, theme::alpha(theme::BRAND, 0.9));
                }
                for p in &points {
                    paint_dot(bounds, window, *p);
                }
            },
        )
        .size_full(),
    )
}

fn paint_dot(bounds: Bounds<Pixels>, window: &mut gpui::Window, p: (f32, f32)) {
    let r = px(2.5);
    let center = bounds.origin + gpui::point(px(p.0), px(p.1));
    let dot_bounds = Bounds::new(gpui::point(center.x - r, center.y - r), gpui::size(r * 2., r * 2.));
    window.paint_quad(gpui::quad(dot_bounds, r, theme::alpha(theme::BRAND, 1.0), px(0.), gpui::transparent_black(), gpui::BorderStyle::default()));
}

fn chart_section(locale: Locale, records: &[&CostRecord]) -> Div {
    let buckets = hourly_call_buckets(records, chrono::Utc::now());
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_4()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "inference.chart.title")))
        .child(call_volume_chart(buckets))
}

// ── 本地模型 tab extra: real `inference.get()` config — see module header
// §3. ───────────────────────────────────────────────────────────────────

fn local_config_row(locale: Locale, cfg: &Loadable<LocalInferenceInfo>) -> Option<Div> {
    let Loadable::Ready(c) = cfg else { return None };
    Some(
        div()
            .flex()
            .items_center()
            .gap_4()
            .p_3()
            .rounded(px(theme::RADIUS_LG))
            .bg(theme::alpha(theme::MUTED, 0.35))
            .text_size(px(12.))
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .child(i18n::t1(locale, "inference.local.enabled", "state", &i18n::t(locale, if c.enabled { "settingsPage.on" } else { "settingsPage.off" })))
            .children((!c.backend.is_empty()).then(|| i18n::t1(locale, "inference.local.backend", "backend", &c.backend)))
            .children((!c.default_model.is_empty()).then(|| i18n::t1(locale, "inference.local.defaultModel", "model", &c.default_model))),
    )
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    maybe_fetch(state, cx);
    let locale = state.locale;
    let g = cx.default_global::<InferenceUsageState>();
    let tab = g.tab;
    let today = g.today.clone();
    let month = g.month.clone();
    let recent = g.recent.clone();
    let local_config = g.local_config.clone();

    let crumb = breadcrumb("inference-breadcrumb", locale, i18n::t(locale, "inference.title"), cx);
    let header = div()
        .child(div().text_size(px(17.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "inference.title")))
        .child(div().mt(px(2.)).text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "inference.subtitle")));

    let tab_items: Vec<TabItem> = PROVIDER_TABS
        .iter()
        .map(|&id| {
            TabItem::new(
                id,
                provider_label(locale, id),
                cx.listener(move |_this, _ev, _window, cx| {
                    cx.default_global::<InferenceUsageState>().tab = id;
                    cx.notify();
                }),
            )
        })
        .collect();
    let tab_row = tabs(tab_items, tab);

    let kpi = kpi_row(locale, &today, &month);

    let chart_body: Div = match &recent {
        Loadable::Loading => skeleton(px(720.), px(150.)),
        Loadable::Failed(err) => div().p_4().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(err.clone()),
        Loadable::Ready(records) if records.is_empty() => div().py_6().child(empty_state("💳", i18n::t(locale, "inference.empty"), None, None::<Div>)),
        Loadable::Ready(records) => {
            let filtered: Vec<&CostRecord> = records.iter().filter(|r| provider_of(&r.model) == tab).collect();
            if filtered.is_empty() {
                div().py_6().child(empty_state("💳", i18n::t(locale, "inference.empty"), None, None::<Div>))
            } else {
                chart_section(locale, &filtered)
            }
        }
    };

    let local_row = if tab == "local" { local_config_row(locale, &local_config) } else { None };

    div().id("inference-page").size_full().overflow_y_scroll().flex().flex_col().items_center().child(
        div()
            .w_full()
            .max_w(px(760.))
            .p_6()
            .flex()
            .flex_col()
            .gap_3p5()
            .child(crumb)
            .child(header)
            .child(tab_row)
            .child(kpi)
            .children(local_row)
            .child(chart_body),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cost_summary_reads_totals() {
        let v = json!({ "available": true, "total_requests": 1204, "total_cost_millicents": 18_600_000 });
        let t = parse_cost_summary(&v);
        assert!(t.available);
        assert_eq!(t.total_requests, 1204);
        assert_eq!(t.total_cost_millicents, 18_600_000);
    }

    #[test]
    fn parse_cost_summary_unavailable_defaults_are_honest_zeros() {
        let t = parse_cost_summary(&json!({ "available": false }));
        assert!(!t.available);
        assert_eq!(t.total_requests, 0);
    }

    #[test]
    fn parse_cost_recent_empty_when_unavailable() {
        let v = json!({ "available": false, "records": [ { "model": "claude-sonnet-4-6" } ] });
        assert!(parse_cost_recent(&v).is_empty());
    }

    #[test]
    fn parse_cost_recent_reads_rows_when_available() {
        let v = json!({ "available": true, "records": [
            { "model": "claude-sonnet-4-6", "created_at": "2026-08-21T14:00:00Z" },
        ]});
        let rows = parse_cost_recent(&v);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model, "claude-sonnet-4-6");
    }

    #[test]
    fn provider_of_classifies_by_real_model_substring() {
        assert_eq!(provider_of("claude-sonnet-4-6"), "claude");
        assert_eq!(provider_of("gpt-4o-mini"), "gpt");
        assert_eq!(provider_of("gemini-2.0-flash"), "gemini");
        assert_eq!(provider_of("qwen2.5-7b-instruct"), "local");
    }

    #[test]
    fn format_ntd_rounds_millicents_to_whole_dollars() {
        assert_eq!(format_ntd(18_600_000), "NT$186");
    }

    #[test]
    fn hourly_call_buckets_counts_within_the_last_12_hours_only() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-21T12:00:00Z").unwrap().with_timezone(&chrono::Utc);
        let a = CostRecord { model: "claude".into(), created_at: "2026-08-21T12:00:00Z".into() };
        let b = CostRecord { model: "claude".into(), created_at: "2026-08-20T00:00:00Z".into() };
        let refs = vec![&a, &b];
        let buckets = hourly_call_buckets(&refs, now);
        assert_eq!(buckets[11], 1);
        assert_eq!(buckets.iter().sum::<u32>(), 1);
    }

    #[test]
    fn parse_inference_get_reads_root_fields() {
        let v = json!({ "enabled": true, "backend": "llama_cpp", "default_model": "qwen2.5-7b.gguf" });
        let cfg = parse_inference_get(&v);
        assert!(cfg.enabled);
        assert_eq!(cfg.backend, "llama_cpp");
        assert_eq!(cfg.default_model, "qwen2.5-7b.gguf");
    }
}
