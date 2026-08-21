// WP-S5b3-G (S5b 第三波) — "預測與驗證" (`nav.rs` id `foresight`, monitor
// area). Visual authority: `commercial/design/duduclaw-s5-viz-pages/
// Foresight.dc.html` (B9) — 迴圈條（預測→執行→觀測→對照）→ 校準長條圖 → 世界
// 模型累積 → 近期任務表, in that exact vertical order.
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, never guessed) ────────────────────────────────────────────
//   `forward.summary {agent_id?}` (dispatch L6256, handler L21378) →
//   `{agents: ForwardAgentSummary[], window_scanned, window_cap}`. Every
//   summary row: agent_id/total/settled/avg_brier/categories(map)/
//   fidelity(map)/last_settled_at (`web/src/lib/api.ts` L1222 — snake_case
//   field names read straight through, no camelCase layer on this bridge).
//   `forward.states {agent_id?, limit}` (dispatch L6272, handler L21521) →
//   `{states: ForwardStateRow[]}` (state_key/agent_id/n_samples/last_updated).
//   `forward.recent {agent_id?, limit}` (dispatch L6260, handler L21410) →
//   `{predictions: ForwardPredictionRow[]}` (prediction_id/task_id/agent_id/
//   round/category/brier/created_at/settled_at/expected_outcome/
//   observed_outcome/task_title).
//   `forward.calibration {agent_id}` (dispatch L6268, handler L21493) →
//   `{calibration: ForwardCalibration}` (n/hit_rate/avg_brier/
//   brier_skill_score/bins[{p_mean,emp_rate,n}]/label). `agent_id` is
//   REQUIRED server-side (errors without it) — see this file's own
//   `maybe_fetch` doc comment for how a specific agent gets chosen.
// All four require manager+ (`require_manager!()` at each dispatch site).
//
// ── Deliberate scope cut vs. `web/src/pages/ForesightPage.tsx` ───────────
// The web page has a SECOND tab ("信念與驗證" / `belief.*`), a per-task
// drill-down dialog (`forward.chain`), and a full agent-picker `<Select>`.
// The task brief for this page names exactly four sections — 迴圈條/校準長
// 條圖/世界模型累積/近期任務表 — which map 1:1 onto `forward.summary`/
// `forward.calibration`/`forward.states`/`forward.recent` and nothing else;
// the belief tab and the chain drill-down are out of this pass's brief, not
// silently dropped. The agent picker is also cut: `forward.summary`/
// `forward.recent`/`forward.states` are called unfiltered (aggregate over
// every agent, matching the canvas's implied "全部員工" default), and
// `forward.calibration` — the one RPC that REQUIRES a specific agent — auto-
// picks the agent with the highest `total` from the just-fetched summary
// (ties broken by `agent_id`, deterministic). This mirrors the web page's
// own behavior when no agent is manually selected for the loop/world-model
// sections (`selectedAgent || undefined`) while giving the calibration
// section a well-defined default instead of hiding it outright — labeled
// honestly ("最多樣本的員工"), never presented as if it were an aggregate.

use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{empty_state, skeleton, table};
use crate::rpc::CallError;
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
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
pub struct AgentSummary {
    pub agent_id: String,
    pub total: u64,
    pub settled: u64,
    /// negligible/moderate/significant/critical → count.
    pub categories: std::collections::BTreeMap<String, u64>,
    /// full/mcp_only/none → count.
    pub fidelity: std::collections::BTreeMap<String, u64>,
}

#[derive(Clone)]
pub struct StateRow {
    pub state_key: String,
    pub n_samples: u64,
}

#[derive(Clone)]
pub struct PredictionRow {
    pub task_title: Option<String>,
    pub task_id: String,
    pub agent_id: String,
    pub round: i64,
    pub category: Option<String>,
    pub brier: Option<f64>,
    pub expected_outcome: Option<String>,
    pub observed_outcome: Option<String>,
}

#[derive(Clone)]
pub struct CalibrationBin {
    pub p_mean: f64,
    pub emp_rate: f64,
    pub n: u64,
}

#[derive(Clone)]
pub struct Calibration {
    pub agent_id: String,
    pub n: u64,
    pub hit_rate: Option<f64>,
    pub avg_brier: Option<f64>,
    pub brier_skill_score: Option<f64>,
    pub bins: Vec<CalibrationBin>,
}

pub struct ForesightState {
    requested: bool,
    pub summary: Loadable<Vec<AgentSummary>>,
    pub states: Loadable<Vec<StateRow>>,
    pub recent: Loadable<Vec<PredictionRow>>,
    /// `None` until `summary` has resolved with at least one agent to pick
    /// from — distinct from `Loading` (still waiting on the network) so the
    /// calibration section can render nothing at all before there is even a
    /// candidate agent, rather than an indefinite skeleton.
    pub calibration: Option<Loadable<Calibration>>,
}

impl Default for ForesightState {
    fn default() -> Self {
        Self { requested: false, summary: Loadable::Loading, states: Loadable::Loading, recent: Loadable::Loading, calibration: None }
    }
}

impl Global for ForesightState {}

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

fn parse_summaries(v: &Value) -> Vec<AgentSummary> {
    v.get("agents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|s| AgentSummary {
            agent_id: s.get("agent_id").and_then(Value::as_str).unwrap_or("").to_string(),
            total: s.get("total").and_then(Value::as_u64).unwrap_or(0),
            settled: s.get("settled").and_then(Value::as_u64).unwrap_or(0),
            categories: s
                .get("categories")
                .and_then(Value::as_object)
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.as_u64().unwrap_or(0))).collect())
                .unwrap_or_default(),
            fidelity: s
                .get("fidelity")
                .and_then(Value::as_object)
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.as_u64().unwrap_or(0))).collect())
                .unwrap_or_default(),
        })
        .collect()
}

fn parse_states(v: &Value) -> Vec<StateRow> {
    v.get("states")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|s| StateRow {
            state_key: s.get("state_key").and_then(Value::as_str).unwrap_or("").to_string(),
            n_samples: s.get("n_samples").and_then(Value::as_u64).unwrap_or(0),
        })
        .collect()
}

fn parse_recent(v: &Value) -> Vec<PredictionRow> {
    v.get("predictions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|r| PredictionRow {
            task_title: r.get("task_title").and_then(Value::as_str).map(str::to_string),
            task_id: r.get("task_id").and_then(Value::as_str).unwrap_or("").to_string(),
            agent_id: r.get("agent_id").and_then(Value::as_str).unwrap_or("").to_string(),
            round: r.get("round").and_then(Value::as_i64).unwrap_or(0),
            category: r.get("category").and_then(Value::as_str).map(str::to_string),
            brier: r.get("brier").and_then(Value::as_f64),
            expected_outcome: r.get("expected_outcome").and_then(Value::as_str).map(str::to_string),
            observed_outcome: r.get("observed_outcome").and_then(Value::as_str).map(str::to_string),
        })
        .collect()
}

fn parse_calibration(agent_id: &str, v: &Value) -> Calibration {
    let cal = v.get("calibration").cloned().unwrap_or(Value::Null);
    Calibration {
        agent_id: agent_id.to_string(),
        n: cal.get("n").and_then(Value::as_u64).unwrap_or(0),
        hit_rate: cal.get("hit_rate").and_then(Value::as_f64),
        avg_brier: cal.get("avg_brier").and_then(Value::as_f64),
        brier_skill_score: cal.get("brier_skill_score").and_then(Value::as_f64),
        bins: cal
            .get("bins")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|b| CalibrationBin {
                p_mean: b.get("p_mean").and_then(Value::as_f64).unwrap_or(0.0),
                emp_rate: b.get("emp_rate").and_then(Value::as_f64).unwrap_or(0.0),
                n: b.get("n").and_then(Value::as_u64).unwrap_or(0),
            })
            .collect(),
    }
}

/// Highest-`total` agent from a summary list — the deterministic auto-pick
/// this file's module doc comment documents (ties broken by `agent_id`).
fn top_agent(summaries: &[AgentSummary]) -> Option<&AgentSummary> {
    summaries.iter().max_by(|a, b| a.total.cmp(&b.total).then_with(|| b.agent_id.cmp(&a.agent_id)))
}

pub fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if state.active_page != "foresight" || cx.default_global::<ForesightState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<ForesightState>().requested = true;
    let tx = state.session_tx.clone();

    // The calibration follow-up call needs `session_tx` too, so it is
    // captured into THIS closure (not read from `state`, which isn't
    // reachable inside a detached `apply` callback) — the standard shape for
    // a "fire call B once call A's response picks B's parameter" chain in
    // this crate (no existing page needed it before this one).
    let tx_for_calibration = tx.clone();
    spawn_call(cx, tx.clone(), "forward.summary", json!({}), move |cx, result| {
        match result {
            Ok(v) => {
                let summaries = parse_summaries(&v);
                let pick = top_agent(&summaries).map(|s| s.agent_id.clone());
                cx.default_global::<ForesightState>().summary = Loadable::Ready(summaries);
                match pick {
                    Some(agent_id) => {
                        cx.default_global::<ForesightState>().calibration = Some(Loadable::Loading);
                        let agent_for_apply = agent_id.clone();
                        spawn_call(
                            cx,
                            tx_for_calibration,
                            "forward.calibration",
                            json!({ "agent_id": agent_id }),
                            move |cx, result| {
                                cx.default_global::<ForesightState>().calibration = Some(
                                    result.map(|v| parse_calibration(&agent_for_apply, &v)).into(),
                                );
                            },
                        );
                    }
                    None => {
                        cx.default_global::<ForesightState>().calibration = Some(Loadable::Ready(Calibration {
                            agent_id: String::new(),
                            n: 0,
                            hit_rate: None,
                            avg_brier: None,
                            brier_skill_score: None,
                            bins: Vec::new(),
                        }));
                    }
                }
            }
            Err(e) => cx.default_global::<ForesightState>().summary = Loadable::Failed(e),
        }
    });
    spawn_call(cx, tx.clone(), "forward.states", json!({"limit": 8}), |cx, result| {
        cx.default_global::<ForesightState>().states = result.map(|v| parse_states(&v)).into();
    });
    spawn_call(cx, tx, "forward.recent", json!({"limit": 20}), |cx, result| {
        cx.default_global::<ForesightState>().recent = result.map(|v| parse_recent(&v)).into();
    });
}

// ── Plain-language token labels ───────────────────────────────────────
// Every token below is read verbatim from the gateway's own enum
// `as_str()` (`crates/duduclaw-gateway/src/prediction/task_forward.rs`
// L40-58/L119-137), never guessed — the zh-TW label follows the canvas's
// own wording where the canvas shows the concept (goal kind), and is this
// file's own best-effort plain phrasing where the canvas doesn't (outcome).

fn goal_kind_label(locale: Locale, kind: &str) -> SharedString {
    let key = match kind {
        "coding_simple" => "foresight.goalKind.codingSimple",
        "coding_complex" => "foresight.goalKind.codingComplex",
        "research_or_qa" => "foresight.goalKind.researchOrQa",
        "planning_or_doc" => "foresight.goalKind.planningOrDoc",
        "ops_or_external" => "foresight.goalKind.opsOrExternal",
        _ => "foresight.goalKind.unknown",
    };
    i18n::t(locale, key)
}

fn outcome_label(locale: Locale, outcome: &str) -> SharedString {
    let key = match outcome {
        "accept" => "foresight.outcome.accept",
        "reject" => "foresight.outcome.reject",
        "blocked" => "foresight.outcome.blocked",
        "escalate" => "foresight.outcome.escalate",
        _ => return outcome.to_string().into(),
    };
    i18n::t(locale, key)
}

fn category_label(locale: Locale, category: Option<&str>) -> SharedString {
    match category {
        None => i18n::t(locale, "forward.pending"),
        Some(c) => {
            let key = match c {
                "negligible" => "forward.category.negligible",
                "moderate" => "forward.category.moderate",
                "significant" => "forward.category.significant",
                "critical" => "forward.category.critical",
                _ => return c.to_string().into(),
            };
            i18n::t(locale, key)
        }
    }
}

// ── Shared bar-row primitive (calibration bins + world-model buckets) ────

fn bar_row(label: SharedString, pct: f64, right_text: SharedString, color: u32) -> Div {
    let pct = pct.clamp(0.0, 100.0) as f32;
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(div().w(px(120.)).flex_shrink_0().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(label))
        .child(
            div().flex_1().h(px(6.)).rounded(px(theme::RADIUS_4XL)).bg(theme::alpha(theme::MUTED, 1.0)).overflow_hidden().child(
                div().h_full().rounded(px(theme::RADIUS_4XL)).w(gpui::relative(pct / 100.0)).bg(theme::alpha(color, 1.0)),
            ),
        )
        .child(
            div()
                .w(px(64.))
                .flex_shrink_0()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(right_text),
        )
}

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

fn stat_cell(value: SharedString, label: SharedString) -> Div {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .gap_0p5()
        .child(div().text_size(px(theme::TEXT_BASE)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(value))
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(label))
}

fn fmt2(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.2}")).unwrap_or_else(|| "—".to_string())
}

// ── Loop strip: 預測 → 執行 → 觀測 → 對照, aggregated across every summary
// row (matches `web/src/pages/ForesightPage.tsx::LoopStrip`'s own
// `summaries.reduce(...)` shape). ──────────────────────────────────────

fn loop_strip(locale: Locale, summaries: &[AgentSummary]) -> Div {
    let total: u64 = summaries.iter().map(|s| s.total).sum();
    let settled: u64 = summaries.iter().map(|s| s.settled).sum();
    let good: u64 = summaries
        .iter()
        .map(|s| s.categories.get("negligible").copied().unwrap_or(0) + s.categories.get("moderate").copied().unwrap_or(0))
        .sum();
    let observed: u64 = summaries
        .iter()
        .map(|s| s.fidelity.get("full").copied().unwrap_or(0) + s.fidelity.get("mcp_only").copied().unwrap_or(0))
        .sum();

    let steps: [(&str, String); 4] = [
        ("foresight.loop.predict", total.to_string()),
        ("foresight.loop.act", total.to_string()),
        ("foresight.loop.observe", observed.to_string()),
        ("foresight.loop.score", if settled > 0 { format!("{good}/{settled}") } else { "0".to_string() }),
    ];

    let mut row = div().flex().flex_wrap().items_center().gap_2();
    for (i, (key, value)) in steps.iter().enumerate() {
        row = row.child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_0p5()
                .px_3()
                .py_1p5()
                .rounded(px(theme::RADIUS_LG))
                .bg(theme::alpha(theme::MUTED, 0.5))
                .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, key)))
                .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::MEDIUM).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(value.clone())),
        );
        if i < steps.len() - 1 {
            row = row.child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child("→"));
        }
    }
    section_card(i18n::t(locale, "foresight.loop.title"), row)
}

fn calibration_section(locale: Locale, cal: &Calibration) -> Div {
    if cal.agent_id.is_empty() {
        return section_card(
            i18n::t(locale, "foresight.calibration.title"),
            div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "forward.empty.title")),
        );
    }
    let stats = div()
        .grid()
        .grid_cols(4)
        .gap_2()
        .child(stat_cell(cal.n.to_string().into(), i18n::t(locale, "foresight.cal.n")))
        .child(stat_cell(
            cal.hit_rate.map(|h| format!("{:.0}%", h * 100.0)).unwrap_or_else(|| "—".to_string()).into(),
            i18n::t(locale, "foresight.cal.hitRate"),
        ))
        .child(stat_cell(fmt2(cal.avg_brier).into(), i18n::t(locale, "forward.metric.brier")))
        .child(stat_cell(fmt2(cal.brier_skill_score).into(), i18n::t(locale, "foresight.cal.bss")));

    let mut body = div().flex().flex_col().gap_2().child(
        div()
            .text_size(px(theme::TEXT_XS))
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .child(i18n::t1(locale, "foresight.calibration.forAgent", "agent", &cal.agent_id)),
    ).child(stats);

    if !cal.bins.is_empty() {
        let mut bins = div().flex().flex_col().gap_1p5().pt_2().border_t_1().border_color(theme::surface_border());
        for b in &cal.bins {
            bins = bins.child(bar_row(
                format!("{:.0}%", b.p_mean * 100.0).into(),
                b.emp_rate * 100.0,
                format!("{:.0}% · n={}", b.emp_rate * 100.0, b.n).into(),
                theme::CHART_1,
            ));
        }
        body = body.child(bins);
    }
    section_card(i18n::t(locale, "foresight.calibration.title"), body)
}

fn states_section(locale: Locale, states: &[StateRow]) -> Div {
    if states.is_empty() {
        return section_card(
            i18n::t(locale, "foresight.states.title"),
            div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "forward.empty.title")),
        );
    }
    let max = states.iter().map(|s| s.n_samples).max().unwrap_or(1).max(1);
    let mut body = div().flex().flex_col().gap_1p5();
    for s in states {
        // `state_key` = "<agent>|<goal_kind>|<phase>|<0|1>" (`TaskStateKey::
        // canonical`, `task_forward.rs` L98-106) — the second `|`-segment is
        // the plain-language-able part; the rest stays internal.
        let kind = s.state_key.split('|').nth(1).unwrap_or(&s.state_key);
        body = body.child(bar_row(goal_kind_label(locale, kind), (s.n_samples as f64 / max as f64) * 100.0, s.n_samples.to_string().into(), theme::CHART_2));
    }
    section_card(i18n::t(locale, "foresight.states.title"), body)
}

fn recent_section(locale: Locale, rows: &[PredictionRow]) -> Div {
    if rows.is_empty() {
        return section_card(
            i18n::t(locale, "forward.recent.title"),
            empty_state("🔮", i18n::t(locale, "forward.empty.title"), Some(i18n::t(locale, "foresight.empty.desc")), None::<Div>),
        );
    }
    let headers: Vec<SharedString> = vec![
        i18n::t(locale, "foresight.table.task"),
        i18n::t(locale, "foresight.chain.expected"),
        i18n::t(locale, "foresight.chain.observed"),
        i18n::t(locale, "foresight.recent.result"),
        i18n::t(locale, "forward.metric.brier"),
    ];
    let mut table_rows: Vec<Vec<SharedString>> = Vec::with_capacity(rows.len());
    for r in rows.iter().take(20) {
        let task_label = r.task_title.clone().unwrap_or_else(|| {
            let short: String = r.task_id.chars().take(8).collect();
            short
        });
        table_rows.push(vec![
            format!("{task_label} · {} · R{}", r.agent_id, r.round + 1).into(),
            r.expected_outcome.as_deref().map(|o| outcome_label(locale, o)).unwrap_or_else(|| "—".into()),
            r.observed_outcome.as_deref().map(|o| outcome_label(locale, o)).unwrap_or_else(|| "—".into()),
            category_label(locale, r.category.as_deref()),
            fmt2(r.brier).into(),
        ]);
    }
    section_card(i18n::t(locale, "forward.recent.title"), table(&headers, &table_rows))
}

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    maybe_fetch(state, cx);
    let locale = state.locale;

    if state.ws_state != WsConnState::Authenticated {
        return div().id("foresight-page").size_full().flex().items_center().justify_center().child(empty_state(
            "🔌",
            i18n::t(locale, "native.home.connError.title"),
            Some(i18n::t(locale, "native.home.connError.desc")),
            None::<Div>,
        ));
    }

    let g = cx.default_global::<ForesightState>();
    let summary = g.summary.clone();
    let states = g.states.clone();
    let recent = g.recent.clone();
    let calibration = g.calibration.clone();

    let header = div()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(div().text_size(px(theme::TEXT_XL)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "foresight.title")))
        .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "foresight.subtitle")));

    let mut col = div().id("foresight-page").size_full().overflow_y_scroll().flex().flex_col().gap_4().p_6().child(header);

    match summary {
        Loadable::Loading => {
            col = col.child(skeleton(px(600.), px(60.))).child(skeleton(px(600.), px(120.))).child(skeleton(px(600.), px(120.)));
        }
        Loadable::Failed(msg) => {
            col = col.child(section_card(
                i18n::t(locale, "foresight.error.title"),
                div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(msg),
            ));
        }
        Loadable::Ready(summaries) if summaries.is_empty() => {
            col = col.child(empty_state(
                "🔮",
                i18n::t(locale, "forward.empty.title"),
                Some(i18n::t(locale, "foresight.empty.desc")),
                None::<Div>,
            ));
        }
        Loadable::Ready(summaries) => {
            col = col.child(loop_strip(locale, &summaries));
            if let Some(cal) = &calibration {
                match cal {
                    Loadable::Loading => col = col.child(skeleton(px(600.), px(100.))),
                    Loadable::Failed(msg) => {
                        col = col.child(section_card(
                            i18n::t(locale, "foresight.calibration.title"),
                            div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(msg.clone()),
                        ))
                    }
                    Loadable::Ready(c) => col = col.child(calibration_section(locale, c)),
                }
            }
            col = col.child(match &states {
                Loadable::Loading => skeleton(px(600.), px(100.)),
                Loadable::Failed(msg) => section_card(
                    i18n::t(locale, "foresight.states.title"),
                    div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(msg.clone()),
                ),
                Loadable::Ready(rows) => states_section(locale, rows),
            });
            col = col.child(match &recent {
                Loadable::Loading => skeleton(px(600.), px(160.)),
                Loadable::Failed(msg) => section_card(
                    i18n::t(locale, "forward.recent.title"),
                    div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(msg.clone()),
                ),
                Loadable::Ready(rows) => recent_section(locale, rows),
            });
        }
    }

    col
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_summaries_reads_snake_case_fields() {
        let v = json!({ "agents": [
            { "agent_id": "xiaodu", "total": 10, "settled": 8, "avg_brier": 0.12,
              "categories": {"negligible": 5, "significant": 1}, "fidelity": {"full": 6} },
        ]});
        let s = parse_summaries(&v);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].agent_id, "xiaodu");
        assert_eq!(s[0].total, 10);
        assert_eq!(s[0].categories.get("negligible"), Some(&5));
    }

    #[test]
    fn top_agent_picks_highest_total_ties_broken_by_id() {
        let rows = vec![
            AgentSummary { agent_id: "b".into(), total: 5, ..Default::default() },
            AgentSummary { agent_id: "a".into(), total: 5, ..Default::default() },
            AgentSummary { agent_id: "c".into(), total: 9, ..Default::default() },
        ];
        assert_eq!(top_agent(&rows).unwrap().agent_id, "c");
    }

    #[test]
    fn top_agent_ties_prefer_lexicographically_smaller_id() {
        let rows = vec![
            AgentSummary { agent_id: "zebra".into(), total: 3, ..Default::default() },
            AgentSummary { agent_id: "alpha".into(), total: 3, ..Default::default() },
        ];
        assert_eq!(top_agent(&rows).unwrap().agent_id, "alpha");
    }

    #[test]
    fn top_agent_empty_is_none() {
        assert!(top_agent(&[]).is_none());
    }

    #[test]
    fn goal_kind_label_falls_back_to_unknown_for_unrecognized_token() {
        // Real tokens read from `task_forward.rs::GoalKind::as_str` — a
        // future new variant should degrade to the "unknown" label, not
        // panic or show a raw key.
        let label = goal_kind_label(Locale::En, "some_future_kind");
        assert_eq!(label, i18n::t(Locale::En, "foresight.goalKind.unknown"));
    }

    #[test]
    fn parse_calibration_reads_bins() {
        let v = json!({ "calibration": { "agent_id": "xiaodu", "n": 12, "hit_rate": 0.75,
            "avg_brier": 0.2, "brier_skill_score": 0.1,
            "bins": [{"p_mean": 0.6, "emp_rate": 0.55, "n": 4}] } });
        let c = parse_calibration("xiaodu", &v);
        assert_eq!(c.n, 12);
        assert_eq!(c.bins.len(), 1);
        assert_eq!(c.bins[0].n, 4);
    }

    #[test]
    fn parse_recent_reads_prediction_rows() {
        let v = json!({ "predictions": [
            { "task_id": "abcdef1234", "agent_id": "xiaodu", "round": 0, "category": "moderate",
              "brier": 0.3, "created_at": "2026-08-20T10:00:00Z", "expected_outcome": "accept",
              "observed_outcome": "accept" },
        ]});
        let rows = parse_recent(&v);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].agent_id, "xiaodu");
        assert_eq!(rows[0].expected_outcome.as_deref(), Some("accept"));
    }
}
