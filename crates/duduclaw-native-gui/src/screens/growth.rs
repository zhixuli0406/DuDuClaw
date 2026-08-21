// WP-S5b3-G (S5b 第三波) — "成長" (`nav.rs` id `growth`, already an "AI 員工"
// area item since S5b2-D's reshuffle — this pass wires that existing id to a
// real page instead of `shell.rs`'s generic placeholder).
//
// Visual authority: `commercial/design/duduclaw-s5-viz-pages/Growth.dc.html`
// (B9) — Lv 卡＋XP 進度條＋六格統計磚 → 成就牆 → 昨日戰報, in that order.
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, never guessed) ────────────────────────────────────────────
//   `growth.snapshot` (dispatch L6856, handler L32306) → `{xp, level,
//   xp_into_level, xp_for_next_level, facts: {agents_count, tasks_completed,
//   knowledge_pages, skills_acquired, routines_completed,
//   custom_skills_approved}, achievements: [{id, unlocked, progress_current,
//   progress_denominator, xp_reward, available, unavailable_reason?,
//   unlocked_at?}]}` — no access gate at dispatch (any authed viewer).
//   `growth.daily_report {date?}` (dispatch L6857, handler L32360) →
//   `{date, tasks_completed, cost_cents, most_active_agent, new_knowledge_pages,
//   xp_gained, xp_basis}`. Omitting `date` reports YESTERDAY (a settled,
//   cached day) — exactly the canvas's "昨日戰報" framing, so this page never
//   passes a `date` param (the web page's 7-day archive picker, which DOES
//   pass explicit dates, is out of this pass's scope — see below).
//
// ── Deliberate scope cut vs. `web/src/pages/GrowthPage.tsx` ──────────────
// The web page also renders a 7-day archive tab-picker
// (`DailyReportArchive`) for arbitrary past days. The task brief names
// exactly four sections — XP 進度條/統計磚/成就牆/昨日戰報 — with no archive;
// this page fetches `growth.daily_report` with no `date` (yesterday only)
// and stops there, matching the brief's literal scope rather than porting
// the archive UI.
//
// Achievement id → plain-language name: the gateway only ever sends an id
// (`web/src/components/growth/achievements-def.ts`'s own doc comment: "the
// gateway is the single source of unlock/progress truth and only sends
// ids"). The eight ids that table enumerates are read straight into
// `achievement_label` below; an id this table doesn't know still renders
// (the raw id as its own label) so a future backend achievement can never
// blank out a wall cell.

use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::{json, Value};

use crate::i18n::{self, Locale};
use crate::mds_gpui::{empty_state, skeleton};
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
pub struct Facts {
    pub agents_count: i64,
    pub tasks_completed: i64,
    pub knowledge_pages: i64,
    pub skills_acquired: i64,
    pub routines_completed: i64,
    pub custom_skills_approved: i64,
}

#[derive(Clone)]
pub struct Achievement {
    pub id: String,
    pub unlocked: bool,
    pub available: bool,
}

#[derive(Clone, Default)]
pub struct Snapshot {
    pub level: i64,
    pub xp_into_level: i64,
    pub xp_for_next_level: i64,
    pub facts: Facts,
    pub achievements: Vec<Achievement>,
}

#[derive(Clone)]
pub struct DailyReport {
    pub date: String,
    pub tasks_completed: i64,
    pub cost_cents: i64,
    pub most_active_agent: Option<String>,
    pub new_knowledge_pages: i64,
    pub xp_gained: i64,
}

pub struct GrowthState {
    requested: bool,
    pub snapshot: Loadable<Snapshot>,
    pub daily: Loadable<DailyReport>,
}

impl Default for GrowthState {
    fn default() -> Self {
        Self { requested: false, snapshot: Loadable::Loading, daily: Loadable::Loading }
    }
}

impl Global for GrowthState {}

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

fn parse_snapshot(v: &Value) -> Snapshot {
    let facts = v.get("facts").cloned().unwrap_or(Value::Null);
    let get_i64 = |v: &Value, k: &str| v.get(k).and_then(Value::as_i64).unwrap_or(0);
    Snapshot {
        level: get_i64(v, "level"),
        xp_into_level: get_i64(v, "xp_into_level"),
        xp_for_next_level: get_i64(v, "xp_for_next_level"),
        facts: Facts {
            agents_count: get_i64(&facts, "agents_count"),
            tasks_completed: get_i64(&facts, "tasks_completed"),
            knowledge_pages: get_i64(&facts, "knowledge_pages"),
            skills_acquired: get_i64(&facts, "skills_acquired"),
            routines_completed: get_i64(&facts, "routines_completed"),
            custom_skills_approved: get_i64(&facts, "custom_skills_approved"),
        },
        achievements: v
            .get("achievements")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|a| Achievement {
                id: a.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                unlocked: a.get("unlocked").and_then(Value::as_bool).unwrap_or(false),
                available: a.get("available").and_then(Value::as_bool).unwrap_or(true),
            })
            .collect(),
    }
}

fn parse_daily_report(v: &Value) -> DailyReport {
    DailyReport {
        date: v.get("date").and_then(Value::as_str).unwrap_or("").to_string(),
        tasks_completed: v.get("tasks_completed").and_then(Value::as_i64).unwrap_or(0),
        cost_cents: v.get("cost_cents").and_then(Value::as_i64).unwrap_or(0),
        most_active_agent: v.get("most_active_agent").and_then(Value::as_str).map(str::to_string),
        new_knowledge_pages: v.get("new_knowledge_pages").and_then(Value::as_i64).unwrap_or(0),
        xp_gained: v.get("xp_gained").and_then(Value::as_i64).unwrap_or(0),
    }
}

pub fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if state.active_page != "growth" || cx.default_global::<GrowthState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<GrowthState>().requested = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx.clone(), "growth.snapshot", json!({}), |cx, result| {
        cx.default_global::<GrowthState>().snapshot = result.map(|v| parse_snapshot(&v)).into();
    });
    spawn_call(cx, tx, "growth.daily_report", json!({}), |cx, result| {
        cx.default_global::<GrowthState>().daily = result.map(|v| parse_daily_report(&v)).into();
    });
}

/// The eight ids `growth.rs` (gateway) currently emits — mirrors
/// `web/src/components/growth/achievements-def.ts`'s own comment block
/// ("Mirrors the id set in W5-BE"). An unrecognized id falls back to
/// itself, never blanking a wall cell.
fn achievement_label(locale: Locale, id: &str) -> SharedString {
    let key = match id {
        "first_agent" => "growth.ach.firstAgent",
        "first_task_done" => "growth.ach.firstTaskDone",
        "tasks_100" => "growth.ach.tasks100",
        "knowledge_100" => "growth.ach.knowledge100",
        "skills_10" => "growth.ach.skills10",
        "inbox_zero_streak_7" => "growth.ach.inboxZeroStreak7",
        "custom_skill_first" => "growth.ach.customSkillFirst",
        "custom_skill_saved_100h" => "growth.ach.customSkillSaved100h",
        _ => return id.to_string().into(),
    };
    i18n::t(locale, key)
}

fn stat_brick(value: SharedString, label: SharedString) -> Div {
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
        .child(div().text_size(px(theme::TEXT_BASE)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(value))
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(label))
}

fn level_card(locale: Locale, s: &Snapshot) -> Div {
    let span = s.xp_for_next_level.max(1);
    let pct = ((s.xp_into_level as f32 / span as f32) * 100.0).clamp(0.0, 100.0);
    let xp_line = i18n::tn(
        locale,
        "growth.xp.into",
        &[("into", &s.xp_into_level.to_string()), ("span", &s.xp_for_next_level.to_string())],
    );

    let bricks = div()
        .grid()
        .grid_cols(3)
        .gap_2()
        .child(stat_brick(s.facts.agents_count.to_string().into(), i18n::t(locale, "growth.fact.agents")))
        .child(stat_brick(s.facts.tasks_completed.to_string().into(), i18n::t(locale, "growth.fact.tasks")))
        .child(stat_brick(s.facts.knowledge_pages.to_string().into(), i18n::t(locale, "growth.fact.knowledge")))
        .child(stat_brick(s.facts.skills_acquired.to_string().into(), i18n::t(locale, "growth.fact.skills")))
        .child(stat_brick(s.facts.routines_completed.to_string().into(), i18n::t(locale, "growth.fact.routines")))
        .child(stat_brick(s.facts.custom_skills_approved.to_string().into(), i18n::t(locale, "growth.fact.customSkills")));

    div()
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::surface_shadow())
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap_1p5()
                        .child(div().text_size(px(theme::TEXT_XS)).font_weight(gpui::FontWeight::MEDIUM).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child("Lv"))
                        .child(div().text_size(px(36.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(s.level.to_string())),
                )
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div().h(px(8.)).rounded(px(theme::RADIUS_4XL)).bg(theme::alpha(theme::MUTED, 1.0)).overflow_hidden().child(
                                div().h_full().rounded(px(theme::RADIUS_4XL)).w(gpui::relative(pct / 100.0)).bg(theme::alpha(theme::CHART_1, 1.0)),
                            ),
                        )
                        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(xp_line)),
                ),
        )
        .child(bricks)
}

fn achievement_cell(locale: Locale, a: &Achievement) -> Div {
    let dimmed = !a.unlocked || !a.available;
    let icon_bg = if a.unlocked { theme::alpha(theme::CHART_1, 0.15) } else { theme::alpha(theme::MUTED, 1.0) };
    let icon_fg = if a.unlocked { theme::CHART_1 } else { theme::MUTED_FOREGROUND };
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap_1p5()
        .p_3()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .when(dimmed, |el| el.opacity(0.45))
        .child(div().size(px(40.)).rounded_full().flex().items_center().justify_center().bg(icon_bg).text_color(theme::alpha(icon_fg, 1.0)).child(if a.unlocked { "★" } else { "☆" }))
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .text_center()
                .child(achievement_label(locale, &a.id)),
        )
}

fn achievements_wall(locale: Locale, s: &Snapshot) -> Div {
    let unlocked = s.achievements.iter().filter(|a| a.unlocked).count();
    let total = s.achievements.len();
    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "growth.achievements.title")))
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::tn(locale, "growth.achievements.unlockedCount", &[("unlocked", &unlocked.to_string()), ("total", &total.to_string())])),
        );
    let mut grid = div().grid().grid_cols(4).gap_2p5();
    for a in &s.achievements {
        grid = grid.child(achievement_cell(locale, a));
    }
    div()
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::surface_shadow())
        .child(header)
        .child(grid)
}

fn cents_to_dollars(cents: i64) -> String {
    format!("NT${}", cents / 100)
}

fn daily_report_card(locale: Locale, daily: &Loadable<DailyReport>) -> Div {
    let mut title_row = div()
        .flex()
        .items_center()
        .justify_between()
        .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "growth.report.archive.title")));

    let body: Div = match daily {
        Loadable::Loading => skeleton(px(560.), px(80.)),
        Loadable::Failed(msg) => div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(msg.clone()),
        Loadable::Ready(r) => {
            // Canvas's own "2026-08-20 的結算" framing — the settled day
            // `growth.daily_report` reported (yesterday, since this page
            // never passes an explicit `date`).
            title_row = title_row.child(
                div()
                    .text_size(px(theme::TEXT_XS))
                    .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                    .child(i18n::t1(locale, "growth.report.forDate", "date", &r.date)),
            );
            let mut bricks = div()
                .grid()
                .grid_cols(4)
                .gap_2()
                .child(stat_brick(r.tasks_completed.to_string().into(), i18n::t(locale, "growth.report.tasksCompleted")))
                .child(stat_brick(cents_to_dollars(r.cost_cents).into(), i18n::t(locale, "growth.report.cost")))
                .child(stat_brick(r.new_knowledge_pages.to_string().into(), i18n::t(locale, "growth.report.newKnowledge")))
                .child(stat_brick(format!("+{}", r.xp_gained).into(), i18n::t(locale, "growth.report.xpGained")));
            if let Some(agent) = &r.most_active_agent {
                bricks = bricks.child(stat_brick(agent.clone().into(), i18n::t(locale, "growth.report.mostActive")));
            }
            div().flex().flex_col().gap_2().child(bricks)
        }
    };
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_4()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::surface_shadow())
        .child(title_row)
        .child(body)
}

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    maybe_fetch(state, cx);
    let locale = state.locale;

    if state.ws_state != WsConnState::Authenticated {
        return div().id("growth-page").size_full().flex().items_center().justify_center().child(empty_state(
            "🔌",
            i18n::t(locale, "native.home.connError.title"),
            Some(i18n::t(locale, "native.home.connError.desc")),
            None::<Div>,
        ));
    }

    let g = cx.default_global::<GrowthState>();
    let snapshot = g.snapshot.clone();
    let daily = g.daily.clone();

    let header = div()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(div().text_size(px(theme::TEXT_XL)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "nav.growth")))
        .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "nav.growth.desc")));

    let mut col = div().id("growth-page").size_full().overflow_y_scroll().flex().flex_col().gap_4().p_6().child(header);

    match snapshot {
        Loadable::Loading => col = col.child(skeleton(px(700.), px(180.))).child(skeleton(px(700.), px(220.))),
        Loadable::Failed(msg) => {
            col = col.child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(msg))
        }
        Loadable::Ready(s) => {
            col = col.child(level_card(locale, &s)).child(achievements_wall(locale, &s));
        }
    }
    col = col.child(daily_report_card(locale, &daily));
    col
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_snapshot_reads_facts_and_achievements() {
        let v = json!({
            "level": 7, "xp_into_level": 1240, "xp_for_next_level": 2000,
            "facts": { "agents_count": 3, "tasks_completed": 186, "knowledge_pages": 94,
                       "skills_acquired": 12, "routines_completed": 58, "custom_skills_approved": 1 },
            "achievements": [
                { "id": "first_agent", "unlocked": true, "available": true },
                { "id": "tasks_100", "unlocked": false, "available": true },
            ],
        });
        let s = parse_snapshot(&v);
        assert_eq!(s.level, 7);
        assert_eq!(s.facts.tasks_completed, 186);
        assert_eq!(s.achievements.len(), 2);
        assert!(s.achievements[0].unlocked);
    }

    #[test]
    fn achievement_label_falls_back_to_raw_id_for_unknown() {
        let label = achievement_label(Locale::En, "some_future_id");
        assert_eq!(label.to_string(), "some_future_id");
    }

    #[test]
    fn achievement_label_resolves_all_eight_known_ids() {
        for id in [
            "first_agent", "first_task_done", "tasks_100", "knowledge_100",
            "skills_10", "inbox_zero_streak_7", "custom_skill_first", "custom_skill_saved_100h",
        ] {
            let label = achievement_label(Locale::ZhTw, id);
            assert_ne!(label.to_string(), id, "id {id} did not resolve to a real label");
        }
    }

    #[test]
    fn parse_daily_report_reads_all_fields() {
        let v = json!({
            "date": "2026-08-20", "tasks_completed": 14, "cost_cents": 3800,
            "most_active_agent": "xiaodu", "new_knowledge_pages": 3, "xp_gained": 86,
            "xp_basis": "some note",
        });
        let r = parse_daily_report(&v);
        assert_eq!(r.date, "2026-08-20");
        assert_eq!(r.cost_cents, 3800);
        assert_eq!(r.most_active_agent.as_deref(), Some("xiaodu"));
    }

    #[test]
    fn cents_to_dollars_drops_cents() {
        assert_eq!(cents_to_dollars(3800), "NT$38");
    }
}
