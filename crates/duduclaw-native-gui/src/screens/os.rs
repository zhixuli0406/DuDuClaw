// WP-S5b3-G (S5b 第三波) — "OS" (`nav.rs` id `os`, monitor area, new this
// wave). Visual authority: `commercial/design/duduclaw-s5-viz-pages/Os.dc.
// html` (B9, KDE 兩層式) — AI 員工總覽卡 → 自動化範本卡 → 主動關懷成效
// （四象限統計＋近期事件表）, in that order.
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, never guessed) ────────────────────────────────────────────
//   `agents.list {}` → `{agents: [...]}` — reused only for `display_name`/
//   `role` (`os.status` itself only carries bare `agent_id`, same gap
//   `web/src/pages/OsPage.tsx::displayNameOf` papers over with its own
//   `useAgentsStore` join). Parsed via the existing `screens::agents_data::
//   parse_agents_list` / `AgentListItem` — this crate's established
//   cross-sibling-module shape (see `agents_summary.rs`'s own imports).
//   `os.status {}` (dispatch L6785, handler L32750) → `{edition, quota:
//   {limit, used}, agents: [{agent_id, os_native, watch:{paths,events,
//   dropped}, frontmost:{poll_secs,running}, footprint, proactive:{enabled,
//   base_threshold, max_per_hour}, induced_rules_count}]}`. Admin-gated.
//   `os.gate.recent {n}` (dispatch L6793, handler L32888) → `{recent:
//   [{ts,agent,event,score,threshold,interruptibility,decision,reason}],
//   quadrants:{correct_detection,false_alarm,missed_need,non_response,
//   correct_silence,unknown}}`. Admin-gated.
//
// ── Deliberate scope cut vs. `web/src/pages/OSPage.tsx` ───────────────────
// The web page also has a live perception-event WS tail (`os.events.*`,
// `os.events.entry` push) and an on-demand "environment doctor"
// (`os.doctor.run`). The task brief names exactly three sections — 員工
//總覽卡/自動化範本卡/主動關懷統計＋事件表 — which this page covers via
// `os.status` + a static (non-RPC) automation-template gallery + `os.gate.
// recent`; the live tail and the doctor are out of this pass's brief.
// Per-agent settings edits (`os.settings.update`) are also NOT wired — this
// is a read-only report page here, matching every other B9 monitor page in
// this wave (write actions on this data model stay on the web dashboard).
// The two automation-template cards are static "組裝不真按" gallery cards
// (a real "套用" would create/edit an `autopilot` rule — a decision-class
// write this pass deliberately does not wire, same discipline the S5b2-E
// catalog-card pages already established for their own "組裝不真按"
// actions).

use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::{json, Value};

use crate::i18n::{self, Locale};
use crate::mds_gpui::{empty_state, skeleton, table};
use crate::rpc::CallError;
use crate::screens::agents::role_label;
use crate::screens::agents_data::{parse_agents_list, AgentListItem};
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

#[derive(Clone)]
pub struct OsAgentRow {
    pub agent_id: String,
    pub os_native: bool,
}

#[derive(Clone, Default)]
pub struct OsQuota {
    pub limit: Option<i64>,
    pub used: i64,
}

#[derive(Clone)]
pub struct OsStatus {
    pub quota: OsQuota,
    pub agents: Vec<OsAgentRow>,
}

#[derive(Clone)]
pub struct GateRow {
    pub ts: String,
    pub agent: String,
    pub event: String,
    pub score: f64,
    pub threshold: f64,
    pub decision: String,
    pub reason: String,
}

#[derive(Clone, Default)]
pub struct GateQuadrants {
    pub correct_detection: i64,
    pub false_alarm: i64,
    pub missed_need: i64,
    pub correct_silence: i64,
    pub non_response: i64,
}

#[derive(Clone)]
pub struct GateRecent {
    pub quadrants: GateQuadrants,
    pub recent: Vec<GateRow>,
}

pub struct OsState {
    requested: bool,
    pub agents_list: Loadable<Vec<AgentListItem>>,
    pub status: Loadable<OsStatus>,
    pub gate: Loadable<GateRecent>,
}

impl Default for OsState {
    fn default() -> Self {
        Self { requested: false, agents_list: Loadable::Loading, status: Loadable::Loading, gate: Loadable::Loading }
    }
}

impl Global for OsState {}

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

fn parse_status(v: &Value) -> OsStatus {
    let quota = v.get("quota").cloned().unwrap_or(Value::Null);
    OsStatus {
        quota: OsQuota {
            limit: quota.get("limit").and_then(Value::as_i64),
            used: quota.get("used").and_then(Value::as_i64).unwrap_or(0),
        },
        agents: v
            .get("agents")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|a| OsAgentRow {
                agent_id: a.get("agent_id").and_then(Value::as_str).unwrap_or("").to_string(),
                os_native: a.get("os_native").and_then(Value::as_bool).unwrap_or(false),
            })
            .collect(),
    }
}

fn parse_gate_recent(v: &Value) -> GateRecent {
    let q = v.get("quadrants").cloned().unwrap_or(Value::Null);
    let gi = |k: &str| q.get(k).and_then(Value::as_i64).unwrap_or(0);
    GateRecent {
        quadrants: GateQuadrants {
            correct_detection: gi("correct_detection"),
            false_alarm: gi("false_alarm"),
            missed_need: gi("missed_need"),
            correct_silence: gi("correct_silence"),
            non_response: gi("non_response"),
        },
        recent: v
            .get("recent")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|r| GateRow {
                ts: r.get("ts").and_then(Value::as_str).unwrap_or("").to_string(),
                agent: r.get("agent").and_then(Value::as_str).unwrap_or("").to_string(),
                event: r.get("event").and_then(Value::as_str).unwrap_or("").to_string(),
                score: r.get("score").and_then(Value::as_f64).unwrap_or(0.0),
                threshold: r.get("threshold").and_then(Value::as_f64).unwrap_or(0.0),
                decision: r.get("decision").and_then(Value::as_str).unwrap_or("").to_string(),
                reason: r.get("reason").and_then(Value::as_str).unwrap_or("").to_string(),
            })
            .collect(),
    }
}

pub fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if state.active_page != "os" || cx.default_global::<OsState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<OsState>().requested = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx.clone(), "agents.list", json!({}), |cx, result| {
        cx.default_global::<OsState>().agents_list = result.map(|v| parse_agents_list(&v)).into();
    });
    spawn_call(cx, tx.clone(), "os.status", json!({}), |cx, result| {
        cx.default_global::<OsState>().status = result.map(|v| parse_status(&v)).into();
    });
    spawn_call(cx, tx, "os.gate.recent", json!({"n": 50}), |cx, result| {
        cx.default_global::<OsState>().gate = result.map(|v| parse_gate_recent(&v)).into();
    });
}

fn section_card(title: SharedString, desc: Option<SharedString>, body: Div) -> Div {
    let mut head = div().flex().flex_col().gap_0p5().child(
        div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(title),
    );
    if let Some(d) = desc {
        head = head.child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(d));
    }
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
        .child(head)
        .child(body)
}

/// One employee card — display_name/role from `agents.list`, `os_native`/
/// quota-blocked reasoning from `os.status`.
fn agent_card(locale: Locale, agent: Option<&AgentListItem>, os_row: &OsAgentRow, quota_full: bool) -> Div {
    let name: SharedString = agent.map(|a| a.display_name.clone()).unwrap_or_else(|| os_row.agent_id.clone()).into();
    let role: SharedString = agent.map(|a| role_label(locale, &a.role)).unwrap_or_default();
    let (dot, status_text) = if os_row.os_native {
        (theme::SUCCESS, i18n::t(locale, "os.card.enabled"))
    } else if quota_full {
        (theme::MUTED_FOREGROUND, i18n::t(locale, "os.card.disabledQuotaFull"))
    } else {
        (theme::MUTED_FOREGROUND, i18n::t(locale, "os.card.disabled"))
    };
    div()
        .flex()
        .flex_col()
        .gap_1()
        .p_3p5()
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(theme::SURFACE_RAISED, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::MEDIUM).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(name))
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(role))
        .child(
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .child(div().size(px(6.)).rounded_full().bg(theme::alpha(dot, 1.0)))
                .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(dot, 1.0)).child(status_text)),
        )
}

fn overview_section(locale: Locale, status: &OsStatus, agents_list: &Loadable<Vec<AgentListItem>>) -> Div {
    let quota_line = match status.quota.limit {
        None => i18n::t1(locale, "os.quota.unlimited", "used", &status.quota.used.to_string()),
        Some(limit) => {
            let text = i18n::tn(locale, "os.quota.limited", &[("used", &status.quota.used.to_string()), ("limit", &limit.to_string())]);
            if status.quota.used >= limit {
                i18n::tn(locale, "os.quota.limitedFull", &[("used", &status.quota.used.to_string()), ("limit", &limit.to_string())])
            } else {
                text
            }
        }
    };
    let quota_full = status.quota.limit.map(|l| status.quota.used >= l).unwrap_or(false);
    let empty_list: Vec<AgentListItem> = Vec::new();
    let list_ref = match agents_list {
        Loadable::Ready(v) => v,
        _ => &empty_list,
    };

    let mut grid = div().grid().grid_cols(2).gap_3();
    for row in &status.agents {
        let found = list_ref.iter().find(|a| a.id == row.agent_id);
        grid = grid.child(agent_card(locale, found, row, quota_full && !row.os_native));
    }
    section_card(i18n::t(locale, "os.section.overview"), Some(quota_line), grid)
}

/// Two static "assembly, not real" automation templates (canvas: 新檔案自動
/// 處理 / 切到特定 App 提醒) — no RPC, `套用` is a disabled honest stub. See
/// this file's module doc comment for why a write is deliberately not wired.
fn automation_templates_section(locale: Locale) -> Div {
    fn card(locale: Locale, title_key: &str, desc_key: &str, agent_key: &str) -> Div {
        div()
            .flex()
            .flex_col()
            .gap_1p5()
            .p_3p5()
            .rounded(px(theme::RADIUS_LG))
            .bg(theme::alpha(theme::SURFACE_RAISED, 1.0))
            .border_1()
            .border_color(theme::surface_border())
            .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::MEDIUM).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, title_key)))
            .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, desc_key)))
            .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, agent_key)))
            .child(
                div()
                    .mt_1()
                    .self_start()
                    .px_2p5()
                    .h(px(26.))
                    .flex()
                    .items_center()
                    .rounded(px(theme::RADIUS_MD))
                    .bg(theme::alpha(theme::MUTED, 0.5))
                    .text_size(px(theme::TEXT_XS))
                    .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                    .cursor(gpui::CursorStyle::OperationNotAllowed)
                    .child(i18n::t(locale, "os.template.apply")),
            )
    }

    let grid = div()
        .grid()
        .grid_cols(2)
        .gap_3()
        .child(card(locale, "os.template.newFile.title", "os.template.newFile.desc", "os.template.newFile.agent"))
        .child(card(locale, "os.template.appSwitch.title", "os.template.appSwitch.desc", "os.template.appSwitch.agent"));
    section_card(i18n::t(locale, "os.section.templates"), Some(i18n::t(locale, "os.section.templates.desc")), grid)
}

fn quadrant_cell(value: i64, label: SharedString, color: u32) -> Div {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .gap_0p5()
        .child(div().text_size(px(theme::TEXT_BASE)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(color, 1.0)).child(value.to_string()))
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).text_center().child(label))
}

fn decision_label(locale: Locale, decision: &str) -> SharedString {
    match decision {
        "allow" => i18n::t(locale, "os.gate.decision.allow"),
        "suppress" => i18n::t(locale, "os.gate.decision.suppress"),
        other => other.to_string().into(),
    }
}

fn gate_section(locale: Locale, gate: &GateRecent) -> Div {
    let q = &gate.quadrants;
    let quadrants = div()
        .flex()
        .gap_2()
        .child(quadrant_cell(q.correct_detection, i18n::t(locale, "os.gate.quadrant.correctDetection"), theme::SUCCESS))
        .child(quadrant_cell(q.correct_silence, i18n::t(locale, "os.gate.quadrant.correctSilence"), theme::SUCCESS))
        .child(quadrant_cell(q.false_alarm, i18n::t(locale, "os.gate.quadrant.falseAlarm"), theme::WARNING))
        .child(quadrant_cell(q.missed_need, i18n::t(locale, "os.gate.quadrant.missedNeed"), theme::WARNING))
        .child(quadrant_cell(q.non_response, i18n::t(locale, "os.gate.quadrant.nonResponse"), theme::MUTED_FOREGROUND));

    let body = if gate.recent.is_empty() {
        div().child(quadrants).child(empty_state("📡", i18n::t(locale, "os.gate.empty.title"), Some(i18n::t(locale, "os.gate.empty.desc")), None::<Div>))
    } else {
        let headers: Vec<SharedString> = vec![
            i18n::t(locale, "os.gate.col.time"),
            i18n::t(locale, "os.gate.col.agent"),
            i18n::t(locale, "os.gate.col.event"),
            i18n::t(locale, "os.gate.col.score"),
            i18n::t(locale, "os.gate.col.decision"),
            i18n::t(locale, "os.gate.col.reason"),
        ];
        let mut rows: Vec<Vec<SharedString>> = Vec::with_capacity(gate.recent.len().min(20));
        for r in gate.recent.iter().take(20) {
            rows.push(vec![
                short_time(&r.ts).into(),
                r.agent.clone().into(),
                r.event.clone().into(),
                format!("{:.2} / {:.2}", r.score, r.threshold).into(),
                decision_label(locale, &r.decision),
                if r.reason.is_empty() { "—".into() } else { r.reason.clone().into() },
            ]);
        }
        div().flex().flex_col().gap_3().child(quadrants).child(table(&headers, &rows))
    };
    section_card(i18n::t(locale, "os.section.gate"), Some(i18n::t(locale, "os.section.gate.desc")), body)
}

/// `"2026-08-21T13:45:00Z"` → `"13:45"` — ISO 8601 is pure ASCII (same
/// reasoning `dashboard.rs::short_time` documents).
fn short_time(ts: &str) -> String {
    if ts.len() >= 16 && ts.as_bytes().get(10) == Some(&b'T') {
        ts[11..16].to_string()
    } else {
        ts.to_string()
    }
}

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    maybe_fetch(state, cx);
    let locale = state.locale;

    if state.ws_state != WsConnState::Authenticated {
        return div().id("os-page").size_full().flex().items_center().justify_center().child(empty_state(
            "🔌",
            i18n::t(locale, "native.home.connError.title"),
            Some(i18n::t(locale, "native.home.connError.desc")),
            None::<Div>,
        ));
    }

    let g = cx.default_global::<OsState>();
    let agents_list = g.agents_list.clone();
    let status = g.status.clone();
    let gate = g.gate.clone();

    let header = div()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(div().text_size(px(theme::TEXT_XL)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "nav.os")))
        .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "nav.os.desc")));

    let mut col = div().id("os-page").size_full().overflow_y_scroll().flex().flex_col().gap_4().p_6().child(header);

    match status {
        Loadable::Loading => col = col.child(skeleton(px(700.), px(140.))),
        Loadable::Failed(msg) => {
            col = col.child(section_card(
                i18n::t(locale, "os.section.overview"),
                None,
                div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(msg),
            ))
        }
        Loadable::Ready(s) if s.agents.is_empty() => {
            col = col.child(empty_state("🤖", i18n::t(locale, "os.empty.title"), Some(i18n::t(locale, "os.empty.desc")), None::<Div>))
        }
        Loadable::Ready(s) => {
            col = col.child(overview_section(locale, &s, &agents_list)).child(automation_templates_section(locale));
        }
    }

    col = col.child(match &gate {
        Loadable::Loading => skeleton(px(700.), px(160.)),
        Loadable::Failed(msg) => section_card(
            i18n::t(locale, "os.section.gate"),
            None,
            div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(msg.clone()),
        ),
        Loadable::Ready(g) => gate_section(locale, g),
    });

    col
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_status_reads_quota_and_agents() {
        let v = json!({
            "edition": "personal",
            "quota": { "limit": 1, "used": 1 },
            "agents": [{ "agent_id": "xiaodu", "os_native": true }, { "agent_id": "acai", "os_native": false }],
        });
        let s = parse_status(&v);
        assert_eq!(s.quota.limit, Some(1));
        assert_eq!(s.agents.len(), 2);
        assert!(s.agents[0].os_native);
    }

    #[test]
    fn parse_status_unlimited_quota_is_none() {
        let v = json!({ "quota": { "limit": null, "used": 3 }, "agents": [] });
        assert_eq!(parse_status(&v).quota.limit, None);
    }

    #[test]
    fn parse_gate_recent_reads_quadrants_and_rows() {
        let v = json!({
            "quadrants": { "correct_detection": 42, "false_alarm": 1, "missed_need": 0,
                           "correct_silence": 118, "non_response": 2, "unknown": 0 },
            "recent": [{ "ts": "2026-08-21T14:02:00Z", "agent": "xiaodu", "event": "app_switch",
                         "score": 0.82, "threshold": 0.70, "interruptibility": 0.5,
                         "decision": "allow", "reason": "test" }],
        });
        let g = parse_gate_recent(&v);
        assert_eq!(g.quadrants.correct_detection, 42);
        assert_eq!(g.recent.len(), 1);
        assert_eq!(g.recent[0].decision, "allow");
    }

    #[test]
    fn short_time_extracts_hh_mm() {
        assert_eq!(short_time("2026-08-21T14:02:00Z"), "14:02");
        assert_eq!(short_time("not-a-timestamp"), "not-a-timestamp");
    }
}
