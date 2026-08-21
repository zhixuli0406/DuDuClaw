// WP-S5b2-F (S5b 第二波) — Screen "執行紀錄" (`nav.rs` id `runs`, already a
// top-level "任務與目標" area item since S5b1-A rebuild — this pass wires
// the screen; the `shell.rs` match arm is a sibling package's scope per
// this task's "側欄/nav/shell.rs 不歸你動" boundary).
//
// Visual authority: `commercial/design/duduclaw-s5-work-pages/Runs.dc.
// html` (B4) — filter row + 唯讀表格 + 點列展開詳情 **inline, not a
// persistent right column** (B4's own prescription, quoted in this task's
// brief). Functional reference: `web/src/pages/RunsPage.tsx` (a resizable
// two-pane master/detail layout) — this page deliberately does NOT
// reproduce that split; see `runs_data.rs`'s header comment for the exact
// RPC shapes and `render_expanded`'s doc comment for what the inline panel
// shows.
//
// ── Honest deviations from the design canvas ─────────────────────────────
// 1. Status dots: the canvas draws four colors (green/amber/red + an
//    implied fourth), but the real `RunStatus` the gateway ever emits has
//    exactly THREE values (`completed`/`running`/`no_reply` — verified
//    against `web/src/lib/run-transcript.ts`'s own `runStatusMeta`, which
//    the web dashboard itself only ever handles three ways too). The
//    canvas's amber "需要人工"/red "失敗" rows are mockup flavor, not a
//    real state this RPC can report — dropped rather than fabricated.
// 2. "篩選 AI 員工"/"時間範圍" are pills, not the canvas's inert badge-
//    style buttons — both ARE live: agent filtering is a real
//    `runs.list {agent_id}` param, time-range is a real client-side filter
//    (`runs_data::matches_time_range`, no server-side param exists).
//    "搜尋摘要內容…" is also live — client-side substring match
//    (`matches_search`), since `runs.list` has no `q` param either.
// 3. Non-admin default scope: this page has no client-side "am I admin"
//    signal (same gap `screens::files` documents). `runs.list` REQUIRES
//    `agent_id` for non-admins (`check_agent_filter!` in `handlers.rs`) —
//    if exactly one agent is visible (the common non-admin shape), that
//    agent is auto-selected; otherwise the call is sent with no filter
//    (correct for admins, and for a scoped user with >1 visible agent it
//    surfaces the real "agent_id parameter is required" error rather than
//    guessing which one they meant).

use gpui::{div, prelude::*, px, Context, Div, Entity, Global, SharedString, Stateful};
use serde_json::json;
use std::collections::HashMap;
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, empty_state, skeleton, BadgeVariant};
use crate::rpc::CallError;
use crate::screens::agents_data::{self, AgentListItem};
use crate::screens::runs_data::{
    duration_secs, format_duration_parts, last_assistant_text, last_todo_snapshot, matches_search,
    matches_time_range, parse_run_detail, parse_runs_list, status_meta, tool_steps, RunDetail, RunSummary, TimeRange,
};
use crate::text_field::TextField;
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

pub use crate::screens::dashboard::Loadable;

/// Proper-noun channel names — same table `web/src/pages/RunsPage.tsx`
/// hardcodes (not translated; a brand name reads the same in every
/// locale).
const CHANNEL_NAMES: &[(&str, &str)] = &[
    ("telegram", "Telegram"),
    ("discord", "Discord"),
    ("line", "LINE"),
    ("slack", "Slack"),
    ("whatsapp", "WhatsApp"),
    ("feishu", "Feishu"),
    ("googlechat", "Google Chat"),
    ("msteams", "Teams"),
    ("webchat", "WebChat"),
];

fn channel_label(locale: Locale, ch: &str) -> SharedString {
    if let Some((_, name)) = CHANNEL_NAMES.iter().find(|(k, _)| *k == ch) {
        return SharedString::from(*name);
    }
    if ch == "other" {
        i18n::t(locale, "runs.channel.other")
    } else {
        SharedString::from(ch.to_string())
    }
}

// ── State ──────────────────────────────────────────────────────────────

pub struct RunsState {
    requested_agents: bool,
    pub agents: Loadable<Vec<AgentListItem>>,
    pub agent_filter: Option<String>,
    pub time_range: TimeRange,
    pub search: Entity<TextField>,
    last_fetch_key: Option<String>,
    pub runs: Loadable<Vec<RunSummary>>,
    pub expanded: Option<String>,
    pub detail_cache: HashMap<String, Loadable<RunDetail>>,
}

impl RunsState {
    fn new(cx: &mut gpui::App) -> Self {
        Self {
            requested_agents: false,
            agents: Loadable::Loading,
            agent_filter: None,
            time_range: TimeRange::All,
            search: TextField::new(cx, i18n::t(Locale::ZhTw, "runs.search.placeholder"), false, ""),
            last_fetch_key: None,
            runs: Loadable::Loading,
            expanded: None,
            detail_cache: HashMap::new(),
        }
    }
}

impl Global for RunsState {}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<RunsState>() {
        let state = RunsState::new(cx);
        cx.set_global(state);
    }
}

/// Auto-scope heuristic — see this module's header comment, deviation #3.
fn effective_agent_filter(state: &RunsState) -> Option<String> {
    if let Some(id) = &state.agent_filter {
        return Some(id.clone());
    }
    match &state.agents {
        Loadable::Ready(v) if v.len() == 1 => Some(v[0].id.clone()),
        _ => None,
    }
}

// ── Fetch orchestration ──────────────────────────────────────────────

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    if !cx.global::<RunsState>().requested_agents {
        cx.global_mut::<RunsState>().requested_agents = true;
        let tx = state.session_tx.clone();
        spawn_call(cx, tx, "agents.list", json!({}), |cx, result| {
            cx.global_mut::<RunsState>().agents = result.map(|v| agents_data::parse_agents_list(&v)).into();
        });
    }

    let agent = effective_agent_filter(cx.global::<RunsState>());
    let key = agent.clone().unwrap_or_else(|| "__all__".to_string());
    if cx.global::<RunsState>().last_fetch_key.as_deref() == Some(key.as_str()) {
        return;
    }
    cx.global_mut::<RunsState>().last_fetch_key = Some(key.clone());
    cx.global_mut::<RunsState>().runs = Loadable::Loading;

    let mut params = json!({});
    if let Some(a) = &agent {
        params["agent_id"] = json!(a);
    }
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "runs.list", params, move |cx, result| {
        let g = cx.global_mut::<RunsState>();
        if g.last_fetch_key.as_deref() == Some(key.as_str()) {
            g.runs = result.map(|v| parse_runs_list(&v)).into();
        }
    });
}

fn maybe_fetch_detail(run_id: &str, state: &RootView, cx: &mut Context<RootView>) {
    if cx.global::<RunsState>().detail_cache.contains_key(run_id) {
        return;
    }
    cx.global_mut::<RunsState>().detail_cache.insert(run_id.to_string(), Loadable::Loading);
    let tx = state.session_tx.clone();
    let id = run_id.to_string();
    let id_for_apply = id.clone();
    spawn_call(cx, tx, "runs.get", json!({ "run_id": id }), move |cx, result| {
        cx.global_mut::<RunsState>().detail_cache.insert(id_for_apply, result.map(|v| parse_run_detail(&v)).into());
    });
}

fn spawn_call(
    cx: &mut Context<RootView>,
    session_tx: tokio_mpsc::UnboundedSender<SessionCommand>,
    method: &'static str,
    params: serde_json::Value,
    apply: impl FnOnce(&mut Context<RootView>, Result<serde_json::Value, String>) + 'static,
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
        CallError::Rejected(v) => v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()),
    }
}

// ── Filter pills ───────────────────────────────────────────────────────

fn pill(id: SharedString, label: SharedString, active: bool, cx: &mut Context<RootView>, on_click: impl Fn(&mut RootView, &mut Context<RootView>) + 'static) -> Stateful<Div> {
    div()
        .id(id)
        .px_2p5()
        .py_1()
        .rounded(px(theme::RADIUS_4XL))
        .text_size(px(theme::TEXT_XS))
        .cursor_pointer()
        .when(active, |s| s.bg(theme::alpha(theme::BRAND, 0.14)).text_color(theme::alpha(theme::BRAND, 1.0)).font_weight(gpui::FontWeight::MEDIUM))
        .when(!active, |s| s.bg(theme::alpha(theme::MUTED, 0.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)))
        .child(label)
        .on_click(cx.listener(move |this, _ev, _window, cx| on_click(this, cx)))
}

fn agent_pills(locale: Locale, cx: &mut Context<RootView>) -> Div {
    let (agent_filter, agents) = {
        let g = cx.global::<RunsState>();
        (g.agent_filter.clone(), g.agents.clone())
    };
    let mut row = div().flex().flex_wrap().gap_1p5().child(pill(
        "runs-agent-all".into(),
        i18n::t(locale, "runs.allAgents"),
        agent_filter.is_none(),
        cx,
        |_this, cx| {
            let g = cx.global_mut::<RunsState>();
            g.agent_filter = None;
            g.expanded = None;
            cx.notify();
        },
    ));
    if let Loadable::Ready(rows) = &agents {
        for a in rows {
            let id = a.id.clone();
            let active = agent_filter.as_deref() == Some(id.as_str());
            let label: SharedString = a.display_name.clone().into();
            let row_id: SharedString = format!("runs-agent-{}", a.id).into();
            let id_for_click = id.clone();
            row = row.child(pill(row_id, label, active, cx, move |_this, cx| {
                let g = cx.global_mut::<RunsState>();
                g.agent_filter = Some(id_for_click.clone());
                g.expanded = None;
                cx.notify();
            }));
        }
    }
    row
}

fn time_range_pills(locale: Locale, cx: &mut Context<RootView>) -> Div {
    let current = cx.global::<RunsState>().time_range;
    let opt = |id: &'static str, key: &'static str, target: TimeRange, cx: &mut Context<RootView>| {
        pill(id.into(), i18n::t(locale, key), current == target, cx, move |_this, cx| {
            cx.global_mut::<RunsState>().time_range = target;
            cx.notify();
        })
    };
    div()
        .flex()
        .gap_1p5()
        .child(opt("runs-range-all", "runs.timeRange.all", TimeRange::All, cx))
        .child(opt("runs-range-today", "runs.timeRange.today", TimeRange::Today, cx))
        .child(opt("runs-range-7d", "runs.timeRange.last7", TimeRange::Last7Days, cx))
}

// ── Row rendering ──────────────────────────────────────────────────────

fn hhmm(rfc3339: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .map(|dt| dt.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_else(|_| "—".to_string())
}

fn duration_label(locale: Locale, secs: i64) -> SharedString {
    let (min, sec) = format_duration_parts(secs);
    if min > 0 {
        i18n::tn(locale, "runs.duration.minSec", &[("min", &min.to_string()), ("sec", &sec.to_string())])
    } else {
        i18n::t1(locale, "runs.duration.sec", "sec", &sec.to_string())
    }
}

fn summary_row(locale: Locale, agent_name: &str, r: &RunSummary, expanded: bool, is_last: bool, cx: &mut Context<RootView>) -> Stateful<Div> {
    let (color, status_suffix) = status_meta(&r.status);
    let dur = duration_secs(&r.started_at, r.ended_at.as_deref());
    let run_id = r.id.clone();

    let mut row = div()
        .id(SharedString::from(format!("runs-row-{}", r.id)))
        .flex()
        .items_center()
        .gap_3()
        .px_3p5()
        .py_2p5()
        .cursor_pointer()
        .hover(|s| s.bg(theme::alpha(theme::MUTED, 0.3)))
        .child(div().w(px(50.)).flex_shrink_0().font_family("SF Mono").text_size(px(11.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(hhmm(&r.started_at)))
        .child(
            div()
                .w(px(180.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap_1()
                .text_size(px(theme::TEXT_SM))
                .child(div().text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(SharedString::from(agent_name.to_string())))
                .child(div().text_size(px(11.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(SharedString::from(format!("· {}", channel_label(locale, &r.channel)))))
        )
        .child(
            div()
                .w(px(110.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap_1p5()
                .text_size(px(theme::TEXT_XS))
                .child(div().w(px(8.)).h(px(8.)).rounded(px(4.)).bg(theme::alpha(color, 1.0)))
                .child(i18n::t(locale, &format!("runs.status.{status_suffix}"))),
        )
        .child(div().flex_1().min_w_0().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).overflow_hidden().child(if r.preview.is_empty() { i18n::t(locale, "runs.preview.empty") } else { SharedString::from(r.preview.clone()) }))
        .child(div().w(px(70.)).flex_shrink_0().text_size(px(11.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).children(dur.map(|s| duration_label(locale, s))))
        .child(div().w(px(20.)).flex_shrink_0().text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(if expanded { "▾" } else { "▸" }));

    if !is_last || expanded {
        row = row.border_b_1().border_color(theme::border());
    }
    row.on_click(cx.listener(move |this, _ev, _window, cx| {
        let g = cx.global_mut::<RunsState>();
        g.expanded = if g.expanded.as_deref() == Some(run_id.as_str()) { None } else { Some(run_id.clone()) };
        cx.notify();
        let _ = this;
    }))
}

/// The inline-expand panel (B4: "非常駐右欄") — tool-step trail + the real
/// TodoWrite snapshot (when this run touched one) + the last assistant
/// reply, quoted. Never a persistent side column.
fn expanded_panel(locale: Locale, detail: &Loadable<RunDetail>) -> Div {
    let body = match detail {
        Loadable::Loading => div().flex().flex_col().gap_2().child(skeleton(px(600.), px(16.))).child(skeleton(px(600.), px(16.))),
        Loadable::Failed(err) => div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(SharedString::from(err.clone())),
        Loadable::Ready(d) => {
            let steps = tool_steps(&d.events);
            let mut col = div().flex().flex_col().gap_2p5();
            if !steps.is_empty() {
                let mut list = div().flex().flex_col().gap_1();
                for s in &steps {
                    let glyph = match s.ok {
                        Some(true) => "✓",
                        Some(false) => "✗",
                        None => "•",
                    };
                    let color = match s.ok {
                        Some(true) => theme::SUCCESS,
                        Some(false) => theme::DESTRUCTIVE,
                        None => theme::MUTED_FOREGROUND,
                    };
                    list = list.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(px(theme::TEXT_XS))
                            .child(div().text_color(theme::alpha(color, 1.0)).child(glyph))
                            .child(div().text_color(theme::alpha(theme::FOREGROUND, 1.0)).font_family("SF Mono").child(SharedString::from(s.label.clone()))),
                    );
                }
                col = col.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().text_size(px(11.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t1(locale, "runs.steps", "count", &steps.len().to_string())))
                        .child(list),
                );
            }
            if let Some(snapshot) = last_todo_snapshot(&d.events) {
                col = col.child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t1(locale, "runs.todo.snapshot", "label", &snapshot)));
            }
            if let Some(text) = last_assistant_text(&d.events) {
                col = col.child(
                    div()
                        .rounded(px(theme::RADIUS_LG))
                        .bg(theme::alpha(theme::SURFACE, 1.0))
                        .border_1()
                        .border_color(theme::surface_border())
                        .px_3()
                        .py_2()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(SharedString::from(text)),
                );
            }
            if steps.is_empty() && last_todo_snapshot(&d.events).is_none() && last_assistant_text(&d.events).is_none() {
                col = col.child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "runs.transcript.empty")));
            }
            col
        }
    };
    div()
        .px(px(56.))
        .py_3()
        .bg(theme::alpha(theme::MUTED, 0.2))
        .border_b_1()
        .border_color(theme::border())
        .child(body)
        .child(div().pt_2().text_size(px(10.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.8)).child(i18n::t(locale, "runs.transcript.note")))
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch(state, cx);

    let locale = state.locale;
    let search_entity = cx.global::<RunsState>().search.clone();
    let query = search_entity.read(cx).content.clone();
    let time_range = cx.global::<RunsState>().time_range;
    let expanded = cx.global::<RunsState>().expanded.clone();
    let runs = cx.global::<RunsState>().runs.clone();
    let agents = match &cx.global::<RunsState>().agents {
        Loadable::Ready(v) => v.clone(),
        _ => Vec::new(),
    };

    if let Some(id) = &expanded {
        maybe_fetch_detail(id, state, cx);
    }

    let agent_name = |id: &str| -> String { agents.iter().find(|a| a.id == id).map(|a| a.display_name.clone()).unwrap_or_else(|| id.to_string()) };

    let header = div()
        .flex()
        .items_center()
        .gap_2()
        .child(div().text_size(px(theme::TEXT_XL)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "nav.runs")))
        .children(match &runs {
            Loadable::Ready(v) => Some(div().text_size(px(theme::TEXT_XS)).font_family("SF Mono").text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(v.len().to_string())),
            _ => None,
        });
    let subtitle = div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "runs.empty.hint"));

    let controls = div()
        .flex()
        .items_center()
        .flex_wrap()
        .gap_2()
        .child(div().w(px(220.)).child(search_entity))
        .child(agent_pills(locale, cx))
        .child(time_range_pills(locale, cx));

    let now = chrono::Local::now();
    let filtered: Vec<&RunSummary> = match &runs {
        Loadable::Ready(v) => v
            .iter()
            .filter(|r| matches_time_range(&r.started_at, time_range, now))
            .filter(|r| matches_search(r, &agent_name(&r.agent_id), &channel_label(locale, &r.channel), &query))
            .collect(),
        _ => Vec::new(),
    };

    let body = match &runs {
        Loadable::Loading => div().flex().flex_col().gap_2().child(skeleton(px(760.), px(44.))).child(skeleton(px(760.), px(44.))).child(skeleton(px(760.), px(44.))),
        Loadable::Failed(err) => div().p_4().child(badge(SharedString::from(err.clone()), BadgeVariant::Destructive)),
        Loadable::Ready(all) if all.is_empty() => div().child(empty_state("📜", i18n::t(locale, "runs.empty"), Some(i18n::t(locale, "runs.empty.hint")), None::<Div>)),
        Loadable::Ready(_) if filtered.is_empty() => div().child(empty_state("📜", i18n::t(locale, "runs.empty.filtered"), None, None::<Div>)),
        Loadable::Ready(_) => {
            let header_row = div()
                .flex()
                .items_center()
                .gap_3()
                .px_3p5()
                .py_2()
                .bg(theme::alpha(theme::MUTED, 0.35))
                .border_b_1()
                .border_color(theme::border())
                .text_size(px(11.))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(div().w(px(50.)).flex_shrink_0().child(i18n::t(locale, "runs.col.time")))
                .child(div().w(px(180.)).flex_shrink_0().child(i18n::t(locale, "runs.col.source")))
                .child(div().w(px(110.)).flex_shrink_0().child(i18n::t(locale, "runs.col.status")))
                .child(div().flex_1().child(i18n::t(locale, "runs.col.summary")))
                .child(div().w(px(70.)).flex_shrink_0().child(i18n::t(locale, "runs.col.duration")))
                .child(div().w(px(20.)).flex_shrink_0());
            let n = filtered.len();
            let mut card = div().rounded(px(theme::RADIUS_XL)).overflow_hidden().bg(theme::alpha(theme::SURFACE, 1.0)).border_1().border_color(theme::surface_border()).flex().flex_col().child(header_row);
            for (i, r) in filtered.iter().enumerate() {
                let is_expanded = expanded.as_deref() == Some(r.id.as_str());
                card = card.child(summary_row(locale, &agent_name(&r.agent_id), r, is_expanded, i + 1 == n, cx));
                if is_expanded {
                    let detail = cx.global::<RunsState>().detail_cache.get(&r.id).cloned().unwrap_or(Loadable::Loading);
                    card = card.child(expanded_panel(locale, &detail));
                }
            }
            card
        }
    };

    div()
        .id("runs-page")
        .size_full()
        .overflow_y_scroll()
        .child(
            div()
                .max_w(px(1040.))
                .mx_auto()
                .flex()
                .flex_col()
                .gap_3()
                .child(header)
                .child(subtitle)
                .child(controls)
                .child(body),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hhmm_formats_or_dashes_on_malformed_input() {
        assert_eq!(hhmm("not-a-date"), "—");
        assert!(hhmm("2026-08-21T14:32:00Z").len() == 5);
    }
}
