// WP-S6b3-P (S6b 第三波, 2026-08-22) — "安全審計" (`Secaudit.dc.html`, B4
// flat findings table + filter row). A "進階設定" drill-down leaf
// (`active_page == "secaudit"`, no `nav.rs` entry — wired from
// `manage_advanced.rs`'s 安全審計 row by this same pass).
//
// ── RPC shape (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, never guessed) ────────────────────────────────────────────
//   `secaudit.reports {}` (dispatch L6187, handler `handle_secaudit_reports`
//   L20700, `require_manager!()`) → `{"reports": [SecauditReportRow]}`,
//   newest first (`secaudit_reports::list_reports`'s own contract, mirrored
//   verbatim by `web/src/lib/api.ts`'s own doc comment on `secaudit.reports`
//   at api.ts:1392-1407). One row: `{file, mtime, repo, started_at,
//   profile_mode, total_findings, by_severity, engines_run_count,
//   engines_missing_count, parse_error}`.
//   `secaudit.report {file}` (dispatch L6191, handler
//   `handle_secaudit_report` L20711, `require_manager!()`) → `{"report":
//   {repo, started_at, profile, engines_run, engines_missing, findings:
//   [SecauditFinding], summary}}`. One finding: `{id, source_engine, kind,
//   severity, title, file, line, snippet, rule_id, evidence[], status}` —
//   field names mirror `crates/duduclaw-cli/src/secaudit/schema.rs`'s
//   snake_case contract verbatim (api.ts:1376-1438's own header comment).
//   `severity` ∈ critical/high/medium/low/info; `status` ∈
//   candidate/confirmed/refuted/needs_human/suppressed.
//
// ── Deliberate deviations from the canvas (documented, not silent) ────────
// 1. **One report, not a cross-scan aggregation.** The canvas's 5 mock rows
//    carry 4 different "發現時間" values, implying findings pooled across
//    several scan runs. There is no RPC that flattens findings across
//    multiple `secaudit.report` files server-side — fanning out N report
//    fetches client-side for one flat table is disproportionate scope for
//    this page. This page shows the SINGLE newest report's own
//    `findings[]` (matching `reports.rs`'s own "picks 'latest' rather than
//    inventing an aggregation" precedent, see that file's header comment) —
//    every row's "發現時間" is honestly the same value (that report's
//    `started_at`), not per-finding fabricated timestamps.
// 2. **Version badge dropped.** The canvas's header shows a "1.62.0" pill —
//    that's `system.version`'s own field, a different RPC family than this
//    page's `secaudit.*` scope (same "wrong RPC family, drop rather than
//    fabricate" reasoning `billing.rs`'s header comment point 1 already
//    applies to its own dropped edition badge).
// 3. **Status labels are honest re-readings of the real 5-value enum**, not
//    the canvas's 3 illustrative strings (待修/追蹤中/已修復) — see
//    `status_display` below for the mapping and why "已修復" (implies a
//    verified fix) isn't one of the real states.
// 4. **"掃描範圍" chip is real** — the selected report's own `repo` field,
//    not the canvas's static "整個 repo" copy.

use gpui::{div, prelude::*, px, Context, Div, Entity, Global, SharedString, Stateful};
use serde_json::{json, Value};

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, empty_state, skeleton, BadgeVariant};
use crate::rpc::CallError;
use crate::screens::manage_advanced_common::breadcrumb;
use crate::text_field::TextField;
use crate::theme;
use crate::ws_status::{self, WsConnState};
use crate::RootView;

pub use crate::screens::dashboard::Loadable;

const SEVERITIES: [&str; 5] = ["critical", "high", "medium", "low", "info"];

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct FindingRow {
    pub severity: String,
    pub file: String,
    pub line: Option<i64>,
    pub title: String,
    pub status: String,
}

#[derive(Clone, Default)]
pub struct ReportDetail {
    pub repo: String,
    pub started_at: String,
    pub findings: Vec<FindingRow>,
}

pub fn parse_report_files(v: &Value) -> Vec<String> {
    v.get("reports")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|r| r.get("file").and_then(Value::as_str).map(str::to_string))
        .collect()
}

pub fn parse_report_detail(v: &Value) -> Option<ReportDetail> {
    let report = v.get("report")?;
    let repo = report.get("repo").and_then(Value::as_str).unwrap_or("").to_string();
    let started_at = report.get("started_at").and_then(Value::as_str).unwrap_or("").to_string();
    let findings = report
        .get("findings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|f| FindingRow {
            severity: f.get("severity").and_then(Value::as_str).unwrap_or("info").to_string(),
            file: f.get("file").and_then(Value::as_str).unwrap_or("").to_string(),
            line: f.get("line").and_then(Value::as_i64),
            title: f.get("title").and_then(Value::as_str).unwrap_or("").to_string(),
            status: f.get("status").and_then(Value::as_str).unwrap_or("candidate").to_string(),
        })
        .collect();
    Some(ReportDetail { repo, started_at, findings })
}

// ── State ──────────────────────────────────────────────────────────────

pub struct SecauditState {
    requested: bool,
    pub report: Loadable<Option<ReportDetail>>,
    pub severity_filter: Option<&'static str>,
    pub search: Entity<TextField>,
}

impl SecauditState {
    fn new(cx: &mut gpui::App) -> Self {
        Self {
            requested: false,
            report: Loadable::Loading,
            severity_filter: None,
            search: TextField::new(cx, i18n::t(Locale::ZhTw, "secaudit.search.placeholder"), false, ""),
        }
    }
}

impl Global for SecauditState {}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<SecauditState>() {
        let state = SecauditState::new(cx);
        cx.set_global(state);
    }
}

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if state.active_page != "secaudit" || cx.global::<SecauditState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<SecauditState>().requested = true;
    let tx = state.session_tx.clone();
    let tx2 = tx.clone();
    cx.spawn(async move |weak, cx| {
        let rx = ws_status::call(&tx, "secaudit.reports", json!({}));
        let outcome = match rx.await {
            Ok(Ok(payload)) => Ok(payload),
            Ok(Err(err)) => Err(describe_call_error(&err)),
            Err(_) => Err("背景連線執行緒已結束".to_string()),
        };
        let files = match outcome {
            Ok(v) => parse_report_files(&v),
            Err(e) => {
                let _ = weak.update(cx, |_view, cx| {
                    cx.global_mut::<SecauditState>().report = Loadable::Failed(e);
                    cx.notify();
                });
                return;
            }
        };
        let Some(latest) = files.into_iter().next() else {
            let _ = weak.update(cx, |_view, cx| {
                cx.global_mut::<SecauditState>().report = Loadable::Ready(None);
                cx.notify();
            });
            return;
        };
        let rx2 = ws_status::call(&tx2, "secaudit.report", json!({"file": latest}));
        let outcome2 = match rx2.await {
            Ok(Ok(payload)) => Ok(payload),
            Ok(Err(err)) => Err(describe_call_error(&err)),
            Err(_) => Err("背景連線執行緒已結束".to_string()),
        };
        let _ = weak.update(cx, |_view, cx| {
            cx.global_mut::<SecauditState>().report = match outcome2 {
                Ok(v) => Loadable::Ready(parse_report_detail(&v)),
                Err(e) => Loadable::Failed(e),
            };
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

fn severity_display(locale: Locale, severity: &str) -> (u32, SharedString) {
    match severity {
        "critical" | "high" => (theme::DESTRUCTIVE, i18n::t(locale, if severity == "critical" { "secaudit.severity.critical" } else { "secaudit.severity.high" })),
        "medium" => (theme::WARNING, i18n::t(locale, "secaudit.severity.medium")),
        _ => (theme::SUCCESS, i18n::t(locale, if severity == "low" { "secaudit.severity.low" } else { "secaudit.severity.info" })),
    }
}

/// Honest re-reading of the real 5-value `status` enum — see module header
/// comment §3 for why this is NOT the canvas's 3 illustrative strings.
fn status_display(locale: Locale, status: &str) -> (BadgeVariant, SharedString) {
    match status {
        "confirmed" => (BadgeVariant::Destructive, i18n::t(locale, "secaudit.status.confirmed")),
        "refuted" => (BadgeVariant::Success, i18n::t(locale, "secaudit.status.refuted")),
        "suppressed" => (BadgeVariant::Secondary, i18n::t(locale, "secaudit.status.suppressed")),
        "needs_human" => (BadgeVariant::Warning, i18n::t(locale, "secaudit.status.needsHuman")),
        _ => (BadgeVariant::Warning, i18n::t(locale, "secaudit.status.candidate")),
    }
}

pub fn format_report_time(ts: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(ts).map(|dt| dt.format("%m-%d %H:%M").to_string()).unwrap_or_else(|_| "—".to_string())
}

pub fn matches_search(row: &FindingRow, query: &str) -> bool {
    if query.trim().is_empty() {
        return true;
    }
    let q = query.trim().to_lowercase();
    row.title.to_lowercase().contains(&q) || row.file.to_lowercase().contains(&q)
}

fn location_label(row: &FindingRow) -> String {
    match row.line {
        Some(l) if l > 0 => format!("{}:{l}", row.file),
        _ => row.file.clone(),
    }
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
        .child(div().w(px(60.)).flex_shrink_0().child(i18n::t(locale, "secaudit.col.severity")))
        .child(div().w(px(220.)).flex_shrink_0().child(i18n::t(locale, "secaudit.col.location")))
        .child(div().flex_1().child(i18n::t(locale, "secaudit.col.summary")))
        .child(div().w(px(90.)).flex_shrink_0().child(i18n::t(locale, "secaudit.col.time")))
        .child(div().w(px(80.)).flex_shrink_0().child(i18n::t(locale, "secaudit.col.status")))
}

fn finding_row(locale: Locale, row: &FindingRow, when: &str, is_last: bool) -> Div {
    let (dot_color, sev_label) = severity_display(locale, &row.severity);
    let (status_variant, status_label) = status_display(locale, &row.status);
    let mut r = div()
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py_2p5()
        .text_size(px(theme::TEXT_SM))
        .child(
            div()
                .w(px(60.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap_1p5()
                .child(div().size(px(7.)).rounded_full().bg(theme::alpha(dot_color, 1.0)))
                .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(sev_label)),
        )
        .child(
            div()
                .w(px(220.))
                .flex_shrink_0()
                .min_w_0()
                .font_family("SF Mono")
                .text_size(px(12.))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .overflow_hidden()
                .child(location_label(row)),
        )
        .child(div().flex_1().min_w_0().text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(SharedString::from(row.title.clone())))
        .child(div().w(px(90.)).flex_shrink_0().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(when.to_string()))
        .child(div().w(px(80.)).flex_shrink_0().child(badge(status_label, status_variant)));
    if !is_last {
        r = r.border_b_1().border_color(theme::border());
    }
    r
}

fn filter_chip(id: SharedString, label: SharedString, selected: bool, on_click: impl Fn(&mut Context<RootView>) + 'static, cx: &mut Context<RootView>) -> Stateful<Div> {
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

    let g = cx.global::<SecauditState>();
    let report = g.report.clone();
    let severity_filter = g.severity_filter;
    let search_entity = g.search.clone();
    let query = search_entity.read(cx).content.clone();

    let crumb = breadcrumb("secaudit-breadcrumb", locale, i18n::t(locale, "secaudit.title"), cx);
    let header = div()
        .child(div().text_size(px(17.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "secaudit.title")))
        .child(div().mt(px(2.)).text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "secaudit.subtitle")));

    let mut sev_chips = div().flex().gap_1p5();
    sev_chips = sev_chips.child(filter_chip(
        "secaudit-sev-all".into(),
        i18n::t(locale, "secaudit.filter.all"),
        severity_filter.is_none(),
        |cx| cx.global_mut::<SecauditState>().severity_filter = None,
        cx,
    ));
    for sev in SEVERITIES {
        let (_, label) = severity_display(locale, sev);
        sev_chips = sev_chips.child(filter_chip(
            format!("secaudit-sev-{sev}").into(),
            label,
            severity_filter == Some(sev),
            move |cx| cx.global_mut::<SecauditState>().severity_filter = Some(sev),
            cx,
        ));
    }

    let repo_label: Option<SharedString> = match &report {
        Loadable::Ready(Some(r)) if !r.repo.is_empty() => Some(i18n::t1(locale, "secaudit.scopeChip", "repo", &r.repo)),
        _ => None,
    };

    let filter_row = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_3()
        .child(sev_chips)
        .child(div().w(px(220.)).child(search_entity))
        .children(repo_label.map(|l| {
            div()
                .px_3()
                .h(px(26.))
                .flex()
                .items_center()
                .rounded(px(theme::RADIUS_LG))
                .bg(theme::alpha(theme::SURFACE, 1.0))
                .border_1()
                .border_color(theme::surface_border())
                .text_size(px(11.5))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(l)
        }));

    let body: Div = match &report {
        Loadable::Loading => div().flex().flex_col().gap_2().child(skeleton(px(900.), px(44.))).child(skeleton(px(900.), px(44.))).child(skeleton(px(900.), px(44.))),
        Loadable::Failed(err) => div().p_4().child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(SharedString::from(err.clone()))),
        Loadable::Ready(None) => div().child(empty_state("🛡️", i18n::t(locale, "secaudit.empty"), None, None::<Div>)),
        Loadable::Ready(Some(detail)) => {
            let when = format_report_time(&detail.started_at);
            let filtered: Vec<&FindingRow> = detail
                .findings
                .iter()
                .filter(|f| severity_filter.is_none_or(|s| f.severity == s))
                .filter(|f| matches_search(f, &query))
                .collect();
            if filtered.is_empty() {
                div().child(empty_state("🛡️", i18n::t(locale, "secaudit.noFindings"), None, None::<Div>))
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
                    card = card.child(finding_row(locale, row, &when, i + 1 == n));
                }
                card
            }
        }
    };

    div()
        .id("secaudit-page")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .items_center()
        .child(div().w_full().max_w(px(1000.)).p_6().flex().flex_col().gap_3p5().child(crumb).child(header).child(filter_row).child(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_report_files_reads_file_names_in_order() {
        let v = json!({ "reports": [ {"file": "b.json"}, {"file": "a.json"} ] });
        assert_eq!(parse_report_files(&v), vec!["b.json", "a.json"]);
    }

    #[test]
    fn parse_report_files_missing_array_is_empty_not_panicking() {
        assert!(parse_report_files(&json!({})).is_empty());
    }

    #[test]
    fn parse_report_detail_reads_repo_started_at_and_findings() {
        let v = json!({ "report": {
            "repo": "DuDuClaw", "started_at": "2026-08-21T09:12:00Z",
            "findings": [
                { "id": "f1", "severity": "high", "file": "src/gateway/mcp.rs", "line": 214, "title": "缺少 scope 檢查", "status": "candidate" },
            ],
        }});
        let d = parse_report_detail(&v).unwrap();
        assert_eq!(d.repo, "DuDuClaw");
        assert_eq!(d.findings.len(), 1);
        assert_eq!(d.findings[0].line, Some(214));
        assert_eq!(d.findings[0].status, "candidate");
    }

    #[test]
    fn parse_report_detail_missing_report_key_is_none() {
        assert!(parse_report_detail(&json!({})).is_none());
    }

    #[test]
    fn location_label_includes_line_only_when_positive() {
        let with_line = FindingRow { severity: "low".into(), file: "a.rs".into(), line: Some(10), title: String::new(), status: String::new() };
        assert_eq!(location_label(&with_line), "a.rs:10");
        let no_line = FindingRow { severity: "low".into(), file: "a.rs".into(), line: None, title: String::new(), status: String::new() };
        assert_eq!(location_label(&no_line), "a.rs");
    }

    #[test]
    fn matches_search_checks_title_and_file() {
        let row = FindingRow { severity: "low".into(), file: "src/mcp.rs".into(), line: None, title: "缺少檢查".into(), status: String::new() };
        assert!(matches_search(&row, ""));
        assert!(matches_search(&row, "mcp"));
        assert!(matches_search(&row, "缺少"));
        assert!(!matches_search(&row, "odoo"));
    }

    #[test]
    fn status_display_maps_all_five_real_values() {
        for s in ["candidate", "confirmed", "refuted", "needs_human", "suppressed"] {
            let (_, label) = status_display(Locale::ZhTw, s);
            assert!(!label.is_empty());
        }
    }
}
