// WP-S6b3-P (S6b 第三波, 2026-08-22) — "可靠性" (`Reliability.dc.html`, B9
// Tabs 切資源 + 單一大圖表, canvas spike 配方). A "進階設定" drill-down leaf
// (`active_page == "reliability"`, no `nav.rs` entry — wired from
// `manage_advanced.rs`'s 可靠性 row by this same pass).
//
// ── RPC shape (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, never guessed) ────────────────────────────────────────────
//   `audit.unified_log {sources: ["channel_failure"], limit}` (dispatch
//   L6148, handler `handle_audit_unified_log` L19874) → `{"events": [...]}`
//   — see `channel_failures.jsonl`'s own parsing at L20028-20103: `severity`
//   is `"warning"` for a real failure row and `"info"` for the paired
//   `channel_recovered` row `channel_alerts::record_recovery` appends to the
//   SAME file (L20069-20070). `event_type` is `"channel.{reason}"` where
//   `reason` is the `FailureReason` enum (RateLimited/Billing/Timeout/
//   BinaryMissing/SpawnError/EmptyResponse/NoAccounts/Unknown — CLAUDE.md's
//   own "Multi-OAuth account rotation" section).
//
// ── Why NOT `audit.reliability_summary` / `audit.evolution_query`
// (the RPC family `web/src/pages/ReliabilityPage.tsx` actually uses) ──────
// Both are per-`agent_id` (mandatory param, `handle_audit_reliability_
// summary` L20301-20304 returns an error without one) — they answer "how
// reliable is agent X", not "which category (通道/整合/CLI 帳號輪替/本地推論)
// is unhealthy", the canvas's own tab axis. There is no agent picker
// anywhere on this canvas. `audit.unified_log`'s real `channel_failure`
// source is the one RPC in this crate that is ACTUALLY category-shaped
// (real failure events, real timestamps) rather than a superficial family-
// name match — same "pick the real fit over the named family" reasoning
// `reports.rs`'s own header comment already applies when it drops a
// `web`-only field for one the backend can't back.
//
// ── Deliberate deviations from the canvas (documented, not silent) ────────
// 1. **Only 通道 is real; 整合/CLI 帳號輪替/本地推論 render an honest stub.**
//    No per-category audit source exists for the other three (`tool_call`
//    is a general MCP-tool-call log, not integration-specific; there is no
//    audit source for account-rotation cooldowns or local-inference
//    failures at all) — same "one real tab, rest an honest stub" precedent
//    `skills.rs`/`memory.rs` already establish.
// 2. **KPI tile 1 is NOT "平均正常運行".** No uptime/heartbeat metric exists
//    anywhere in this RPC family — computing a percentage would be inventing
//    a formula and presenting it as a real measurement. Replaced with
//    "已恢復事件數" (a real count this exact data source provides: rows
//    where `channel_alerts::record_recovery` appended a recovery marker),
//    same position in the KPI row, honestly relabeled.
// 3. **Chart title is "通道事故量" not "通道健康度".** The line series is a
//    real per-hour INCIDENT COUNT (bucketed from real timestamps), not a
//    derived health score — relabeled to match what is actually plotted.

use chrono::Datelike;
use gpui::{div, prelude::*, px, Bounds, Context, Div, Global, Pixels, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{empty_state, skeleton, tabs, TabItem};
use crate::rpc::CallError;
use crate::screens::goals::relative_time;
use crate::screens::manage_advanced_common::breadcrumb;
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

pub use crate::screens::dashboard::Loadable;

const RESOURCE_TABS: [&str; 4] = ["channels", "integrations", "rotation", "localInference"];
const FETCH_LIMIT: u64 = 500;

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ChannelFailureEvent {
    pub timestamp: String,
    /// `true` for a real failure row (`severity == "warning"`), `false` for
    /// a paired `channel_recovered` row (`severity == "info"`).
    pub is_incident: bool,
}

pub fn parse_channel_failures(v: &Value) -> Vec<ChannelFailureEvent> {
    v.get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|e| ChannelFailureEvent {
            timestamp: e.get("timestamp").and_then(Value::as_str).unwrap_or("").to_string(),
            is_incident: e.get("severity").and_then(Value::as_str) == Some("warning"),
        })
        .collect()
}

// ── State ──────────────────────────────────────────────────────────────

pub struct ReliabilityState {
    requested: bool,
    pub tab: &'static str,
    pub events: Loadable<Vec<ChannelFailureEvent>>,
}

impl Default for ReliabilityState {
    fn default() -> Self {
        Self { requested: false, tab: "channels", events: Loadable::Loading }
    }
}

impl Global for ReliabilityState {}

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if state.active_page != "reliability" || cx.default_global::<ReliabilityState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<ReliabilityState>().requested = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "audit.unified_log", json!({"sources": ["channel_failure"], "limit": FETCH_LIMIT}), |cx, result| {
        cx.default_global::<ReliabilityState>().events = result.map(|v| parse_channel_failures(&v)).into();
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

fn resource_label(locale: Locale, id: &str) -> SharedString {
    match id {
        "channels" => i18n::t(locale, "reliability.tab.channels"),
        "integrations" => i18n::t(locale, "reliability.tab.integrations"),
        "rotation" => i18n::t(locale, "reliability.tab.rotation"),
        _ => i18n::t(locale, "reliability.tab.localInference"),
    }
}

// ── KPI tiles ──────────────────────────────────────────────────────────

fn kpi_tile(value: SharedString, label: SharedString, color: Option<u32>) -> Div {
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
        .child(div().text_size(px(19.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(color.unwrap_or(theme::FOREGROUND), 1.0)).child(value))
}

fn is_this_month(ts: &str, now: chrono::DateTime<chrono::Utc>) -> bool {
    chrono::DateTime::parse_from_rfc3339(ts).map(|dt| dt.year() == now.year() && dt.month() == now.month()).unwrap_or(false)
}

fn kpi_row(locale: Locale, events: &[ChannelFailureEvent]) -> Div {
    let now = chrono::Utc::now();
    let incidents_this_month = events.iter().filter(|e| e.is_incident && is_this_month(&e.timestamp, now)).count();
    let last_incident = events.iter().filter(|e| e.is_incident).map(|e| e.timestamp.as_str()).max();
    let recovered_count = events.iter().filter(|e| !e.is_incident).count();

    let last_incident_label: SharedString = match last_incident {
        Some(ts) => relative_time(locale, ts, now),
        None => i18n::t(locale, "reliability.kpi.none"),
    };

    div()
        .flex()
        .gap_2p5()
        .child(kpi_tile(recovered_count.to_string().into(), i18n::t(locale, "reliability.kpi.recovered"), Some(theme::SUCCESS)))
        .child(kpi_tile(incidents_this_month.to_string().into(), i18n::t(locale, "reliability.kpi.incidentsThisMonth"), None))
        .child(kpi_tile(last_incident_label, i18n::t(locale, "reliability.kpi.lastIncident"), Some(theme::WARNING)))
}

// ── Canvas-drawn chart: 通道事故量 · 近 12 小時 (single-series filled line,
// same `PathBuilder::stroke`/`fill` recipe `reports.rs::trend_chart`
// establishes, extended with a filled area under the curve to match this
// canvas's own polygon+polyline look). ─────────────────────────────────

fn hourly_incident_buckets(events: &[ChannelFailureEvent], now: chrono::DateTime<chrono::Utc>) -> [u32; 12] {
    let mut buckets = [0u32; 12];
    for e in events {
        if !e.is_incident {
            continue;
        }
        let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&e.timestamp) else { continue };
        let dt = dt.with_timezone(&chrono::Utc);
        let hours_ago = (now - dt).num_hours();
        if (0..12).contains(&hours_ago) {
            let idx = 11 - hours_ago as usize;
            buckets[idx] += 1;
        }
    }
    buckets
}

fn incident_chart(buckets: [u32; 12]) -> Div {
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
                // Filled area under the curve.
                let mut fill = gpui::PathBuilder::fill();
                fill.move_to(bounds.origin + gpui::point(px(points[0].0), px(HEIGHT)));
                for p in &points {
                    fill.line_to(bounds.origin + gpui::point(px(p.0), px(p.1)));
                }
                fill.line_to(bounds.origin + gpui::point(px(points[points.len() - 1].0), px(HEIGHT)));
                if let Ok(path) = fill.build() {
                    window.paint_path(path, theme::alpha(theme::BRAND, 0.08));
                }
                // Stroke line + dots.
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

fn chart_section(locale: Locale, events: &[ChannelFailureEvent]) -> Div {
    let buckets = hourly_incident_buckets(events, chrono::Utc::now());
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_4()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "reliability.chart.title")))
        .child(incident_chart(buckets))
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    maybe_fetch(state, cx);
    let locale = state.locale;
    let g = cx.default_global::<ReliabilityState>();
    let tab = g.tab;
    let events = g.events.clone();

    let crumb = breadcrumb("reliability-breadcrumb", locale, i18n::t(locale, "reliability.title"), cx);
    let header = div()
        .child(div().text_size(px(17.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "reliability.title")))
        .child(div().mt(px(2.)).text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "reliability.subtitle")));

    let tab_items: Vec<TabItem> = RESOURCE_TABS
        .iter()
        .map(|&id| {
            TabItem::new(
                id,
                resource_label(locale, id),
                cx.listener(move |_this, _ev, _window, cx| {
                    cx.default_global::<ReliabilityState>().tab = id;
                    cx.notify();
                }),
            )
        })
        .collect();
    let tab_row = tabs(tab_items, tab);

    let body: Div = if tab != "channels" {
        div().py_10().child(empty_state("🚧", i18n::t(locale, "reliability.stub"), None, None::<Div>))
    } else {
        match &events {
            Loadable::Loading => div().flex().flex_col().gap_2().child(skeleton(px(720.), px(90.))).child(skeleton(px(720.), px(150.))),
            Loadable::Failed(err) => div().p_4().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(err.clone()),
            Loadable::Ready(rows) => div().flex().flex_col().gap_3p5().child(kpi_row(locale, rows)).child(chart_section(locale, rows)),
        }
    };

    div()
        .id("reliability-page")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .items_center()
        .child(div().w_full().max_w(px(760.)).p_6().flex().flex_col().gap_3p5().child(crumb).child(header).child(tab_row).child(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_channel_failures_classifies_incident_vs_recovery_by_severity() {
        let v = json!({ "events": [
            { "timestamp": "2026-08-19T10:00:00Z", "severity": "warning" },
            { "timestamp": "2026-08-19T11:00:00Z", "severity": "info" },
        ]});
        let rows = parse_channel_failures(&v);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].is_incident);
        assert!(!rows[1].is_incident);
    }

    #[test]
    fn parse_channel_failures_missing_array_is_empty_not_panicking() {
        assert!(parse_channel_failures(&json!({})).is_empty());
    }

    #[test]
    fn hourly_incident_buckets_counts_within_the_last_12_hours_only() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-21T12:00:00Z").unwrap().with_timezone(&chrono::Utc);
        let events = vec![
            ChannelFailureEvent { timestamp: "2026-08-21T12:00:00Z".into(), is_incident: true }, // this hour, bucket 11
            ChannelFailureEvent { timestamp: "2026-08-21T01:00:00Z".into(), is_incident: true },  // 11h ago, bucket 0
            ChannelFailureEvent { timestamp: "2026-08-20T10:00:00Z".into(), is_incident: true },  // outside window
            ChannelFailureEvent { timestamp: "2026-08-21T12:00:00Z".into(), is_incident: false }, // recovery, not counted
        ];
        let buckets = hourly_incident_buckets(&events, now);
        assert_eq!(buckets[11], 1);
        assert_eq!(buckets[0], 1);
        assert_eq!(buckets.iter().sum::<u32>(), 2);
    }

    #[test]
    fn resource_tabs_constant_matches_the_canvas_four_tabs() {
        assert_eq!(RESOURCE_TABS, ["channels", "integrations", "rotation", "localInference"]);
    }
}
