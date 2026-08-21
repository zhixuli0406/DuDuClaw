// WP-S5b3-H (S5b 第三波, 2026-08-21) — Screen "工作時間軸" (`nav.rs` id
// `timeline` — not yet wired; see this task's own "nav.rs 不歸你動" boundary,
// this page's `shell.rs` arm is hung by this same pass per the "D 先掛好分支
// 就直接可達，未掛就自己掛" precedent `screens/shell.rs`'s WP-S5b2-E comment
// already establishes).
//
// Visual authority: `commercial/design/duduclaw-s5-viz-pages/Timeline.dc.
// html` (B10) — top aggregate breakdown bar + label column + horizontally-
// scrolling hour-grid lane chart + "now" marker + legend. Functional
// reference: `web/src/pages/TimelinePage.tsx` (layout NOT copied — see
// `timeline_data.rs`'s header comment for the exact RPC shape and the
// deliberate "color by kind, not by task sub-status" simplification the
// canvas itself already makes).
//
// ── Painting technique (spike_t7_timeline.rs recipe, as instructed) ──────
// One `gpui::canvas()` sized to the FULL time-range width (not just the
// viewport) inside an `.overflow_x_scroll()` + `.track_scroll(...)`
// container — identical shape to the spike's primitive 2. Hour gridlines +
// labels use the spike's own `shape_line`/`ShapedLine::paint` technique;
// row bars/dots are `window.paint_quad` rectangles/rounded-quads colored by
// `timeline_data::kind_color`. Deliberately brute-force (paints every block
// every frame, no scroll-position virtualization) — same trade-off the
// spike's own module doc comment accepts, and this page's row cap
// (`TIMELINE_ROW_CAP` = 2000 server-side, in practice far fewer land in one
// lane) never approaches the spike's proven 500-block stress ceiling.
//
// ── Honest deviations from the canvas ─────────────────────────────────────
// 1. No hover tooltip — per-block mouse hit-testing inside a `canvas()`
//    paint closure is a bigger primitive than this wave's T7 spike proved
//    (it only proved static painting + whole-canvas drag/wheel, not
//    per-shape hit-testing) — out of scope, same "decorative, not fully
//    interactive" scope line the task brief draws for /forks' winner button.
// 2. "now" marker is a solid 1.5px bar, not the canvas's dashed line — this
//    gpui rev's `PathBuilder`/quad API has no dash-stroke primitive (checked
//    against the vendored `path_builder.rs`/`scene.rs`, not assumed).
// 3. Hour labels are drawn every hour uniformly regardless of the selected
//    range (even at 7 days = 168 columns) — literally following the
//    spike_t7_timeline.rs recipe rather than web's adaptive `timeTicks`
//    tick-thinning; the horizontally-scrolling container this task brief
//    asks for is exactly what makes a wide 7-day canvas usable.

use gpui::{div, prelude::*, px, Context, Div, Global, ScrollHandle, SharedString, Stateful};
use serde_json::json;
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, empty_state, skeleton, BadgeVariant};
use crate::rpc::CallError;
use crate::screens::agents_data::{self, AgentListItem};
use crate::screens::timeline_data::{
    build_lanes, kind_breakdown, kind_color, parse_timeline_list, Lane, TimeRange, TimelineListResult,
    LEGEND_KINDS,
};
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

pub use crate::screens::dashboard::Loadable;

const PX_PER_HOUR: f32 = 70.0;
const HEADER_HEIGHT: f32 = 22.0;
const SUBROW_PITCH: f32 = 24.0;
const BAR_H: f32 = 14.0;
const LANE_VPAD: f32 = 7.0;
const DOT_R: f32 = 4.0;
const LABEL_COL_W: f32 = 96.0;

// ── State ──────────────────────────────────────────────────────────────

pub struct TimelineState {
    requested_agents: bool,
    pub agents: Loadable<Vec<AgentListItem>>,
    pub agent_filter: Option<String>,
    pub range: TimeRange,
    last_fetch_key: Option<String>,
    pub result: Loadable<TimelineListResult>,
    pub page_scroll: ScrollHandle,
    pub chart_scroll: ScrollHandle,
}

impl TimelineState {
    fn new() -> Self {
        Self {
            requested_agents: false,
            agents: Loadable::Loading,
            agent_filter: None,
            range: TimeRange::TwentyFourHours,
            last_fetch_key: None,
            result: Loadable::Loading,
            page_scroll: ScrollHandle::new(),
            chart_scroll: ScrollHandle::new(),
        }
    }
}

impl Global for TimelineState {}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<TimelineState>() {
        cx.set_global(TimelineState::new());
    }
}

/// Auto-scope heuristic — same "exactly one visible agent ⇒ auto-select it"
/// precedent `runs.rs::effective_agent_filter` documents (deviation #3 in
/// that module's header comment); this page has no client-side "am I admin"
/// signal either.
fn effective_agent_filter(state: &TimelineState) -> Option<String> {
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
    if !cx.global::<TimelineState>().requested_agents {
        cx.global_mut::<TimelineState>().requested_agents = true;
        let tx = state.session_tx.clone();
        spawn_call(cx, tx, "agents.list", json!({}), |cx, result| {
            cx.global_mut::<TimelineState>().agents = result.map(|v| agents_data::parse_agents_list(&v)).into();
        });
    }

    let agent = effective_agent_filter(cx.global::<TimelineState>());
    let range = cx.global::<TimelineState>().range;
    let key = format!("{}:{}", agent.clone().unwrap_or_else(|| "__all__".to_string()), range.key());
    if cx.global::<TimelineState>().last_fetch_key.as_deref() == Some(key.as_str()) {
        return;
    }
    cx.global_mut::<TimelineState>().last_fetch_key = Some(key.clone());
    cx.global_mut::<TimelineState>().result = Loadable::Loading;

    let to = chrono::Utc::now();
    let from = to - chrono::Duration::hours(range.hours());
    let mut params = json!({ "from": from.to_rfc3339(), "to": to.to_rfc3339() });
    if let Some(a) = &agent {
        params["agent_id"] = json!(a);
    }
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "timeline.list", params, move |cx, result| {
        let g = cx.global_mut::<TimelineState>();
        if g.last_fetch_key.as_deref() == Some(key.as_str()) {
            g.result = result.map(|v| parse_timeline_list(&v)).into();
        }
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

// ── Controls ───────────────────────────────────────────────────────────

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

fn range_pills(locale: Locale, cx: &mut Context<RootView>) -> Div {
    let current = cx.global::<TimelineState>().range;
    let mut row = div().flex().gap_1p5();
    for r in TimeRange::ALL {
        let id: SharedString = format!("timeline-range-{}", r.key()).into();
        let label = i18n::t(locale, &format!("timeline.range.{}", r.key()));
        row = row.child(pill(id, label, current.key() == r.key(), cx, move |_this, cx| {
            cx.global_mut::<TimelineState>().range = r;
            cx.notify();
        }));
    }
    row
}

fn agent_pills(locale: Locale, cx: &mut Context<RootView>) -> Div {
    let (agent_filter, agents) = {
        let g = cx.global::<TimelineState>();
        (g.agent_filter.clone(), g.agents.clone())
    };
    let mut row = div().flex().flex_wrap().gap_1p5().child(pill(
        "timeline-agent-all".into(),
        i18n::t(locale, "timeline.allAgents"),
        agent_filter.is_none(),
        cx,
        |_this, cx| {
            cx.global_mut::<TimelineState>().agent_filter = None;
            cx.notify();
        },
    ));
    if let Loadable::Ready(rows) = &agents {
        for a in rows {
            let id = a.id.clone();
            let active = agent_filter.as_deref() == Some(id.as_str());
            let label: SharedString = a.display_name.clone().into();
            let row_id: SharedString = format!("timeline-agent-{}", a.id).into();
            let id_for_click = id.clone();
            row = row.child(pill(row_id, label, active, cx, move |_this, cx| {
                cx.global_mut::<TimelineState>().agent_filter = Some(id_for_click.clone());
                cx.notify();
            }));
        }
    }
    row
}

/// "回到現在" — snap the horizontally-scrolled chart to its right edge (the
/// window's `to`, always "now" per `maybe_fetch`'s own fetch semantics).
/// `ScrollHandle::set_offset`/`max_offset` are real gpui API, verified
/// against the vendored `elements/div.rs`, not assumed.
fn back_to_now_button(cx: &mut Context<RootView>) -> Stateful<Div> {
    let handle = cx.global::<TimelineState>().chart_scroll.clone();
    div()
        .id("timeline-back-to-now")
        .px_3()
        .py_1p5()
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .cursor_pointer()
        .hover(|s| s.bg(theme::alpha(theme::MUTED, 0.4)))
        .child("⟳")
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            let max = handle.max_offset();
            let cur = handle.offset();
            handle.set_offset(gpui::point(max.x, cur.y));
            cx.notify();
        }))
}

// ── Aggregate bar + legend ─────────────────────────────────────────────

fn aggregate_bar(locale: Locale, lanes: &[Lane]) -> Div {
    let breakdown = kind_breakdown(lanes);
    let mut bar = div().flex().h(px(14.)).rounded(px(7.)).overflow_hidden().w_full();
    if breakdown.is_empty() {
        bar = bar.bg(theme::alpha(theme::MUTED, 0.4));
    } else {
        for (kind, frac) in &breakdown {
            bar = bar.child(div().h_full().w(gpui::relative(*frac)).bg(theme::alpha(kind_color(kind), 1.0)));
        }
    }
    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .p_3()
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .child(div().text_size(px(11.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "timeline.breakdown.hint")))
        .child(bar)
}

fn legend(locale: Locale) -> Div {
    let mut row = div().flex().flex_wrap().gap_4();
    for kind in LEGEND_KINDS {
        row = row.child(
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .text_size(px(11.))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(div().w(px(9.)).h(px(9.)).rounded(px(2.)).bg(theme::alpha(kind_color(kind), 1.0)))
                .child(i18n::t(locale, &format!("timeline.kind.{kind}"))),
        );
    }
    row
}

// ── Chart (label column + canvas) ──────────────────────────────────────

fn lane_tops_heights(lanes: &[Lane]) -> Vec<(f32, f32)> {
    let mut y = HEADER_HEIGHT;
    lanes
        .iter()
        .map(|l| {
            let h = l.sub_row_count as f32 * SUBROW_PITCH + LANE_VPAD * 2.0;
            let top = y;
            y += h;
            (top, h)
        })
        .collect()
}

fn label_column(locale: Locale, lanes: &[Lane], tops_heights: &[(f32, f32)], agents: &Loadable<Vec<AgentListItem>>) -> Div {
    let agent_name = |id: &str| -> String {
        if let Loadable::Ready(rows) = agents {
            if let Some(a) = rows.iter().find(|a| a.id == id) {
                return a.display_name.clone();
            }
        }
        id.to_string()
    };
    let mut col = div().flex_shrink_0().w(px(LABEL_COL_W)).flex().flex_col();
    col = col.child(div().h(px(HEADER_HEIGHT)));
    let _ = locale;
    for (lane, (_, h)) in lanes.iter().zip(tops_heights.iter()) {
        let name = agent_name(&lane.agent_id);
        let initial = name.chars().next().map(|c| c.to_string()).unwrap_or_else(|| "?".to_string());
        col = col.child(
            div()
                .h(px(*h))
                .flex()
                .items_center()
                .gap_1p5()
                .pr_2()
                .border_t_1()
                .border_color(theme::border())
                .child(
                    div()
                        .flex_shrink_0()
                        .w(px(20.))
                        .h(px(20.))
                        .rounded(px(10.))
                        .bg(theme::alpha(theme::BRAND, 0.85))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(10.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0))
                        .child(SharedString::from(initial)),
                )
                .child(div().flex_1().min_w_0().overflow_hidden().text_size(px(11.5)).font_weight(gpui::FontWeight::MEDIUM).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(SharedString::from(name))),
        );
    }
    col
}

#[allow(clippy::too_many_arguments)]
fn paint_hour_grid(bounds: gpui::Bounds<gpui::Pixels>, from_ms: i64, to_ms: i64, content_width: f32, content_height: f32, window: &mut gpui::Window, cx: &mut gpui::App) {
    let hours = ((to_ms - from_ms) as f64 / 3_600_000.0).round().max(1.0) as i64;
    let line_color = theme::alpha(theme::BORDER, theme::BORDER_ALPHA * 2.0);
    for hour in 0..=hours {
        let x = bounds.origin.x + px((hour as f32 / hours as f32) * content_width);
        let line_bounds = gpui::Bounds::new(
            gpui::point(x, bounds.origin.y + px(HEADER_HEIGHT)),
            gpui::size(px(1.0), px((content_height - HEADER_HEIGHT).max(0.0))),
        );
        window.paint_quad(gpui::fill(line_bounds, line_color));

        if hour < hours {
            let ts_ms = from_ms + hour * 3_600_000;
            let label: SharedString = chrono::DateTime::from_timestamp_millis(ts_ms)
                .map(|dt| dt.with_timezone(&chrono::Local).format("%H:%M").to_string())
                .unwrap_or_default()
                .into();
            let font = window.text_style().font();
            let run = gpui::TextRun {
                len: label.len(),
                font,
                color: theme::alpha(theme::MUTED_FOREGROUND, 1.0).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let shaped = window.text_system().shape_line(label, px(theme::TEXT_XS - 1.0), &[run], None);
            let label_origin = gpui::point(x + px(3.0), bounds.origin.y + px(4.0));
            if let Err(e) = shaped.paint(label_origin, px(HEADER_HEIGHT), gpui::TextAlign::Left, None, window, cx) {
                eprintln!("[timeline] hour label paint failed: {e}");
            }
        }
    }
}

fn paint_lanes(bounds: gpui::Bounds<gpui::Pixels>, lanes: &[Lane], tops_heights: &[(f32, f32)], from_ms: i64, to_ms: i64, content_width: f32, window: &mut gpui::Window) {
    let span = (to_ms - from_ms).max(1) as f32;
    let x_for = |ms: i64| -> f32 { ((ms - from_ms) as f32 / span) * content_width };

    for (lane, (top, _)) in lanes.iter().zip(tops_heights.iter()) {
        for p in &lane.rows {
            let y = top + LANE_VPAD + p.sub_row as f32 * SUBROW_PITCH + (SUBROW_PITCH - BAR_H) / 2.0;
            let color = theme::alpha(kind_color(&p.row.kind), if p.running { 1.0 } else { 0.85 });
            if p.instant {
                let cx_ = x_for(p.start_ms);
                let r = px(DOT_R);
                let dot_bounds = gpui::Bounds::new(
                    bounds.origin + gpui::point(px(cx_) - r, px(y + BAR_H / 2.0) - r),
                    gpui::size(r * 2.0, r * 2.0),
                );
                window.paint_quad(gpui::quad(dot_bounds, r, color, px(0.), gpui::transparent_black(), gpui::BorderStyle::default()));
            } else {
                let x1 = x_for(p.start_ms);
                let x2 = x_for(p.end_ms);
                let w = (x2 - x1).max(3.0);
                let bar_bounds = gpui::Bounds::new(bounds.origin + gpui::point(px(x1), px(y)), gpui::size(px(w), px(BAR_H)));
                window.paint_quad(gpui::quad(bar_bounds, px(BAR_H / 2.0), color, px(0.), theme::alpha(theme::SURFACE_BORDER, 1.0), gpui::BorderStyle::default()));
            }
        }
    }

    // "now" marker — solid bar at the right edge (== to_ms). See this
    // module's header comment, deviation #2, for why it isn't dashed.
    let now_x = bounds.origin.x + px(content_width - 1.5);
    let content_height = tops_heights.last().map(|(t, h)| t + h).unwrap_or(HEADER_HEIGHT);
    let now_bounds = gpui::Bounds::new(gpui::point(now_x, bounds.origin.y + px(HEADER_HEIGHT - 4.0)), gpui::size(px(1.5), px((content_height - HEADER_HEIGHT + 4.0).max(0.0))));
    window.paint_quad(gpui::fill(now_bounds, theme::alpha(theme::BRAND, 1.0)));
}

fn chart(lanes: Vec<Lane>, from_ms: i64, to_ms: i64, hours: i64, scroll_handle: &ScrollHandle) -> Stateful<Div> {
    let content_width = (hours as f32 * PX_PER_HOUR).max(1.0);
    let tops_heights = lane_tops_heights(&lanes);
    let content_height = tops_heights.last().map(|(t, h)| t + h).unwrap_or(HEADER_HEIGHT).max(HEADER_HEIGHT + 40.0);

    div()
        .id("timeline-chart-viewport")
        .flex_1()
        .min_w(px(320.))
        .h(px(content_height))
        .overflow_x_scroll()
        .track_scroll(scroll_handle)
        .child(
            gpui::canvas(
                move |_bounds, _window, _cx| {},
                move |bounds, _prepaint, window, cx| {
                    paint_hour_grid(bounds, from_ms, to_ms, content_width, content_height, window, cx);
                    paint_lanes(bounds, &lanes, &tops_heights, from_ms, to_ms, content_width, window);
                },
            )
            .w(px(content_width))
            .h(px(content_height)),
        )
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch(state, cx);

    let locale = state.locale;
    let result = cx.global::<TimelineState>().result.clone();
    let agents = cx.global::<TimelineState>().agents.clone();
    let agent_order: Vec<String> = match &agents {
        Loadable::Ready(v) => v.iter().map(|a| a.id.clone()).collect(),
        _ => Vec::new(),
    };

    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_size(px(theme::TEXT_XL)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "timeline.title")))
                .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "timeline.desc"))),
        )
        .child(div().flex().items_center().gap_2().child(range_pills(locale, cx)).child(back_to_now_button(cx)));

    let controls = agent_pills(locale, cx);

    let body: Div = match &result {
        Loadable::Loading => div().flex().flex_col().gap_2().child(skeleton(px(1000.), px(60.))).child(skeleton(px(1000.), px(200.))),
        Loadable::Failed(err) => div().p_4().child(badge(SharedString::from(err.clone()), BadgeVariant::Destructive)),
        Loadable::Ready(r) if r.rows.is_empty() => {
            div().child(empty_state("📅", i18n::t(locale, "timeline.empty"), Some(i18n::t(locale, "timeline.empty.hint")), None::<Div>))
        }
        Loadable::Ready(r) => {
            let from_ms = chrono::DateTime::parse_from_rfc3339(&r.from).map(|d| d.timestamp_millis()).unwrap_or(0);
            let to_ms = chrono::DateTime::parse_from_rfc3339(&r.to).map(|d| d.timestamp_millis()).unwrap_or(from_ms + 3_600_000);
            let hours = cx.global::<TimelineState>().range.hours();
            let lanes = build_lanes(&r.rows, &agent_order, from_ms, to_ms, to_ms);
            let tops_heights = lane_tops_heights(&lanes);

            let mut col = div().flex().flex_col().gap_3();
            if r.truncated {
                col = col.child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::WARNING, 1.0))
                        .child(i18n::t1(locale, "timeline.truncated", "cap", &r.cap.to_string())),
                );
            }
            col = col.child(aggregate_bar(locale, &lanes));
            if lanes.is_empty() {
                col = col.child(empty_state("📅", i18n::t(locale, "timeline.empty.filtered"), None, None::<Div>));
            } else {
                let scroll_handle = cx.global::<TimelineState>().chart_scroll.clone();
                col = col.child(
                    div()
                        .rounded(px(theme::RADIUS_XL))
                        .p_3()
                        .bg(theme::alpha(theme::SURFACE, 1.0))
                        .border_1()
                        .border_color(theme::surface_border())
                        .flex()
                        .child(label_column(locale, &lanes, &tops_heights, &agents))
                        .child(chart(lanes, from_ms, to_ms, hours, &scroll_handle)),
                );
            }
            col = col.child(legend(locale));
            col
        }
    };

    div()
        .id("timeline-page")
        .size_full()
        .track_scroll(&cx.global::<TimelineState>().page_scroll)
        .overflow_y_scroll()
        .child(div().max_w(px(1200.)).mx_auto().flex().flex_col().gap_3().child(header).child(controls).child(body))
}
