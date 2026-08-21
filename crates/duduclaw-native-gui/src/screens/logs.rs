// WP-S6b3-P (S6b 第三波, 2026-08-22) — "日誌" (`Logs.dc.html`, B4 flat
// table + filter row). A "進階設定" drill-down leaf (`active_page ==
// "logs"`, no `nav.rs` entry — wired from `manage_advanced.rs`'s 日誌 row by
// this same pass).
//
// ── RPC shape (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, never guessed) ────────────────────────────────────────────
//   `audit.unified_log {limit, sources[], severity_filter, agent_id_filter}`
//   (dispatch L6148, handler `handle_audit_unified_log` L19874, no
//   `require_*!()` gate) → `{"events": [{timestamp, source, event_type,
//   agent_id, severity, summary, details}], "source_counts": {...},
//   "total"}`. `source` ∈ security/tool_call/channel_failure/feedback (the
//   4 unified-log categories, `all_sources` L19885); `severity` ∈
//   info/warning/critical (L19949-19952, L19989 — `success` bool maps
//   tool_call rows to info/warning only, never critical).
//   This page fetches once with `{"limit": 200}` (server default cap) and
//   applies every filter (等級/模組/關鍵字) client-side — matches
//   `governance.rs`'s own "one fetch, client-side chip filters" shape (that
//   RPC has no filter params either; this one HAS `severity_filter`/
//   `agent_id_filter` but no keyword param, so a single generous fetch +
//   local filtering is simpler than juggling two filtering strategies).
//
// ── Deliberate deviations from the canvas (documented, not silent) ────────
// 1. **"模組" column = `source`, not a per-component name.** The canvas's
//    mock rows show component-level labels ("gateway", "account_rotator",
//    "goal_loop", …) — no such per-component field exists anywhere in
//    `audit.unified_log`'s schema (only the 4-value `source` enum). Labeled
//    honestly with the real 4-value category (安全/工具呼叫/通道/回饋)
//    instead of inventing finer-grained module names the RPC can't back.
// 2. **"時間範圍" dropdown dropped.** `audit.unified_log` has no time-window
//    parameter (only `limit`) — there is nothing real to filter by. Replaced
//    with a static "最近 N 筆" caption showing the actual fetch size.
// 3. **搜尋 is real** (unlike the decorative placeholder some sibling pages
//    use) — client-side substring match over `summary`, same
//    `Entity<TextField>` primitive `runs.rs`'s own 搜尋摘要內容 box already
//    establishes (see that file's header comment point 2).
// 4. **"已載入最近 N / 24,180 筆" footer** — the canvas's total (24,180) is
//    illustrative sample data; this page shows the REAL `total` field the
//    RPC returns (count of ALL matching rows on disk, not just the fetched
//    page) next to the real fetched-row count.

use gpui::{div, prelude::*, px, Context, Div, Entity, Global, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{empty_state, skeleton};
use crate::rpc::CallError;
use crate::screens::manage_advanced_common::breadcrumb;
use crate::text_field::TextField;
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

pub use crate::screens::dashboard::Loadable;

const FETCH_LIMIT: u64 = 200;
const SOURCES: [&str; 4] = ["security", "tool_call", "channel_failure", "feedback"];
const SEVERITIES: [&str; 3] = ["info", "warning", "critical"];

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct LogRow {
    pub timestamp: String,
    pub source: String,
    pub severity: String,
    pub summary: String,
}

#[derive(Clone, Default)]
pub struct LogsResponse {
    pub events: Vec<LogRow>,
    pub total: i64,
}

pub fn parse_unified_log(v: &Value) -> LogsResponse {
    let events = v
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|e| LogRow {
            timestamp: e.get("timestamp").and_then(Value::as_str).unwrap_or("").to_string(),
            source: e.get("source").and_then(Value::as_str).unwrap_or("").to_string(),
            severity: e.get("severity").and_then(Value::as_str).unwrap_or("info").to_string(),
            summary: e.get("summary").and_then(Value::as_str).unwrap_or("").to_string(),
        })
        .collect();
    LogsResponse { events, total: v.get("total").and_then(Value::as_i64).unwrap_or(0) }
}

// ── State ──────────────────────────────────────────────────────────────

pub struct LogsState {
    requested: bool,
    pub response: Loadable<LogsResponse>,
    pub severity_filter: Option<&'static str>,
    pub source_filter: Option<&'static str>,
    pub search: Entity<TextField>,
}

impl LogsState {
    fn new(cx: &mut gpui::App) -> Self {
        Self {
            requested: false,
            response: Loadable::Loading,
            severity_filter: None,
            source_filter: None,
            search: TextField::new(cx, i18n::t(Locale::ZhTw, "logs.search.placeholder"), false, ""),
        }
    }
}

impl Global for LogsState {}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<LogsState>() {
        let state = LogsState::new(cx);
        cx.set_global(state);
    }
}

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if state.active_page != "logs" || cx.global::<LogsState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<LogsState>().requested = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "audit.unified_log", json!({"limit": FETCH_LIMIT}), |cx, result| {
        cx.global_mut::<LogsState>().response = result.map(|v| parse_unified_log(&v)).into();
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

fn source_label(locale: Locale, source: &str) -> SharedString {
    match source {
        "security" => i18n::t(locale, "logs.source.security"),
        "tool_call" => i18n::t(locale, "logs.source.toolCall"),
        "channel_failure" => i18n::t(locale, "logs.source.channelFailure"),
        "feedback" => i18n::t(locale, "logs.source.feedback"),
        other => other.to_string().into(),
    }
}

fn severity_display(locale: Locale, severity: &str) -> (u32, SharedString) {
    match severity {
        "warning" => (theme::WARNING, i18n::t(locale, "logs.severity.warning")),
        "critical" => (theme::DESTRUCTIVE, i18n::t(locale, "logs.severity.critical")),
        _ => (theme::SUCCESS, i18n::t(locale, "logs.severity.info")),
    }
}

/// RFC3339 → "14:32:08" (time only — the header note explains why: this
/// page's own single-day-ish recency window makes a bare date redundant).
/// "—" when unparseable/empty, same fallback `billing.rs::format_date`
/// establishes.
pub fn format_time(ts: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(ts).map(|dt| dt.format("%H:%M:%S").to_string()).unwrap_or_else(|_| "—".to_string())
}

pub fn matches_search(row: &LogRow, query: &str) -> bool {
    query.trim().is_empty() || row.summary.to_lowercase().contains(&query.trim().to_lowercase())
}

// ── Rows ───────────────────────────────────────────────────────────────

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
        .child(div().w(px(70.)).flex_shrink_0().child(i18n::t(locale, "logs.col.time")))
        .child(div().w(px(70.)).flex_shrink_0().child(i18n::t(locale, "logs.col.severity")))
        .child(div().w(px(110.)).flex_shrink_0().child(i18n::t(locale, "logs.col.module")))
        .child(div().flex_1().child(i18n::t(locale, "logs.col.message")))
}

fn log_row(locale: Locale, row: &LogRow, is_last: bool) -> Div {
    let (dot_color, sev_label) = severity_display(locale, &row.severity);
    let mut r = div()
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py_2p5()
        .text_size(px(theme::TEXT_SM))
        .child(
            div()
                .w(px(70.))
                .flex_shrink_0()
                .font_family("SF Mono")
                .text_size(px(12.))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(format_time(&row.timestamp)),
        )
        .child(
            div()
                .w(px(70.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap_1p5()
                .child(div().size(px(7.)).rounded_full().bg(theme::alpha(dot_color, 1.0)))
                .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(sev_label)),
        )
        .child(
            div()
                .w(px(110.))
                .flex_shrink_0()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(source_label(locale, &row.source)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(SharedString::from(row.summary.clone())),
        );
    if !is_last {
        r = r.border_b_1().border_color(theme::border());
    }
    r
}

fn filter_chip(locale: Locale, id: SharedString, label: SharedString, selected: bool, on_click: impl Fn(&mut Context<RootView>) + 'static, cx: &mut Context<RootView>) -> Stateful<Div> {
    let _ = locale;
    div()
        .id(id)
        .h(px(26.))
        .px_2p5()
        .flex()
        .items_center()
        .rounded(px(theme::RADIUS_4XL))
        .cursor_pointer()
        .text_size(px(11.5))
        .font_weight(gpui::FontWeight::MEDIUM)
        .when(selected, |el| el.bg(theme::alpha(theme::BRAND, 1.0)).text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0)))
        .when(!selected, |el| {
            el.bg(theme::alpha(theme::SURFACE, 1.0)).border_1().border_color(theme::surface_border()).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        })
        .child(label)
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            on_click(cx);
            cx.notify();
        }))
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch(state, cx);
    let locale = state.locale;

    let g = cx.global::<LogsState>();
    let response = g.response.clone();
    let severity_filter = g.severity_filter;
    let source_filter = g.source_filter;
    let search_entity = g.search.clone();
    let query = search_entity.read(cx).content.clone();

    let crumb = breadcrumb("logs-breadcrumb", locale, i18n::t(locale, "logs.title"), cx);
    let header = div()
        .child(div().text_size(px(17.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "logs.title")))
        .child(div().mt(px(2.)).text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "logs.subtitle")));

    let mut severity_chips = div().flex().gap_1p5();
    severity_chips = severity_chips.child(filter_chip(
        locale,
        "logs-sev-all".into(),
        i18n::t(locale, "logs.filter.allSeverity"),
        severity_filter.is_none(),
        |cx| cx.global_mut::<LogsState>().severity_filter = None,
        cx,
    ));
    for sev in SEVERITIES {
        let (_, label) = severity_display(locale, sev);
        severity_chips = severity_chips.child(filter_chip(
            locale,
            format!("logs-sev-{sev}").into(),
            label,
            severity_filter == Some(sev),
            move |cx| cx.global_mut::<LogsState>().severity_filter = Some(sev),
            cx,
        ));
    }

    let mut source_chips = div().flex().gap_1p5();
    source_chips = source_chips.child(filter_chip(
        locale,
        "logs-src-all".into(),
        i18n::t(locale, "logs.filter.allModule"),
        source_filter.is_none(),
        |cx| cx.global_mut::<LogsState>().source_filter = None,
        cx,
    ));
    for src in SOURCES {
        source_chips = source_chips.child(filter_chip(
            locale,
            format!("logs-src-{src}").into(),
            source_label(locale, src),
            source_filter == Some(src),
            move |cx| cx.global_mut::<LogsState>().source_filter = Some(src),
            cx,
        ));
    }

    let filter_row = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_3()
        .child(severity_chips)
        .child(source_chips)
        .child(div().w(px(220.)).child(search_entity));

    let body: Div = match &response {
        Loadable::Loading => div().flex().flex_col().gap_2().child(skeleton(px(900.), px(44.))).child(skeleton(px(900.), px(44.))).child(skeleton(px(900.), px(44.))),
        Loadable::Failed(err) => div().p_4().child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(SharedString::from(err.clone()))),
        Loadable::Ready(resp) => {
            let filtered: Vec<&LogRow> = resp
                .events
                .iter()
                .filter(|r| severity_filter.is_none_or(|s| r.severity == s))
                .filter(|r| source_filter.is_none_or(|s| r.source == s))
                .filter(|r| matches_search(r, &query))
                .collect();
            if filtered.is_empty() {
                div().child(empty_state("📄", i18n::t(locale, "logs.empty"), None, None::<Div>))
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
                    .child(header_row(locale));
                for (i, row) in filtered.into_iter().enumerate() {
                    card = card.child(log_row(locale, row, i + 1 == n));
                }
                card
            }
        }
    };

    let footer: Option<Div> = match &response {
        Loadable::Ready(resp) => Some(
            div()
                .text_size(px(11.5))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .text_center()
                .child(i18n::tn(locale, "logs.footerNote", &[("loaded", &resp.events.len().to_string()), ("total", &resp.total.to_string())])),
        ),
        _ => None,
    };

    div()
        .id("logs-page")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .items_center()
        .child(
            div()
                .w_full()
                .max_w(px(1000.))
                .p_6()
                .flex()
                .flex_col()
                .gap_3p5()
                .child(crumb)
                .child(header)
                .child(filter_row)
                .child(body)
                .children(footer),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_unified_log_reads_events_and_total() {
        let v = json!({
            "events": [
                { "timestamp": "2026-08-21T14:32:08Z", "source": "tool_call", "event_type": "tool.odoo.write.failure", "agent_id": "a1", "severity": "warning", "summary": "timeout" },
            ],
            "source_counts": { "security": 0, "tool_call": 1, "channel_failure": 0, "feedback": 0 },
            "total": 24180,
        });
        let resp = parse_unified_log(&v);
        assert_eq!(resp.events.len(), 1);
        assert_eq!(resp.events[0].source, "tool_call");
        assert_eq!(resp.events[0].severity, "warning");
        assert_eq!(resp.total, 24180);
    }

    #[test]
    fn parse_unified_log_missing_array_is_empty_not_panicking() {
        let resp = parse_unified_log(&json!({}));
        assert!(resp.events.is_empty());
        assert_eq!(resp.total, 0);
    }

    #[test]
    fn format_time_is_hms_or_dash() {
        assert_eq!(format_time("2026-08-21T14:32:08Z"), "14:32:08");
        assert_eq!(format_time("garbage"), "—");
    }

    #[test]
    fn matches_search_is_case_insensitive_substring_or_empty() {
        let row = LogRow { timestamp: String::new(), source: "tool_call".into(), severity: "info".into(), summary: "Odoo write Timeout".into() };
        assert!(matches_search(&row, ""));
        assert!(matches_search(&row, "timeout"));
        assert!(matches_search(&row, "ODOO"));
        assert!(!matches_search(&row, "nomatch"));
    }

    #[test]
    fn severity_display_covers_the_real_three_value_enum() {
        assert_eq!(severity_display(Locale::ZhTw, "critical").0, theme::DESTRUCTIVE);
        assert_eq!(severity_display(Locale::ZhTw, "warning").0, theme::WARNING);
        assert_eq!(severity_display(Locale::ZhTw, "info").0, theme::SUCCESS);
    }
}
