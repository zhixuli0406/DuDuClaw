// WP-S5b3-I (2026-08-21) — 畫布 (`Canvas.dc.html`, B12). "AI 員工推送、你唯讀
// 檢視＋版本歷史" report viewer: a full-bleed content frame with a floating
// control bar (agent picker / version picker / refresh) hovering over it.
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, not guessed) ─────────────────────────────────────────────
//   `agents.list {}` → `handle_agents_list_filtered` (~L5396); parsed via
//     the existing `screens::agents_data::{AgentListItem, parse_agents_list}`
//     (same reuse `world.rs` makes — see that module's own doc comment).
//   `canvas.get {"agent_id","seq"?}` → `handle_canvas_get` (~L40073) →
//     `{ "agent_id", "canvas": { "seq","agent_id","title","html",
//     "updated_at" } | null, "history": [ { "seq","title","updated_at",
//     "bytes" } ] (newest first, ≤5) }`. Mirrors `web/src/lib/api.ts`'s
//     `CanvasGetResult`/`CanvasInfo`/`CanvasVersionMeta` exactly.
//
// ── Content-rendering honesty boundary (read before assuming a bug) ──────
// The web page renders `canvas.html` — server-ammonia-sanitized, arbitrary
// HTML — inside a fully sandboxed `<iframe sandbox="">`. gpui is not a
// browser engine (same "no HTML-sandbox rendering capability" gap `widgets.
// rs`'s own module doc comment documents for its thumbnails): this page
// never parses or paints `canvas.html` at all. Instead it shows the
// STRUCTURED metadata the RPC hands over as real JSON fields — `title`,
// `updated_at`, `seq`, the history list — and an honest placeholder message
// in place of the HTML body itself, plus a genuinely-wired "在瀏覽器開啟"
// button (`cx.open_url`) pointing at the same page's real web-dashboard URL
// (`{api::GATEWAY_BASE_URL}/canvas?agent=<id>`) so the content is still one
// click away, never silently dropped.
//
// ── Canvas fidelity deviations (documented, not silent) ───────────────────
// 1. Agent picker — the mockup shows a `<select>`-style dropdown ("小杜的畫布
//    ▾"). This crate's `mds_gpui` facade has no dropdown/select primitive
//    yet (grep-verified: `mds_gpui/mod.rs` exports badge/button/card/dialog/
//    empty_state/skeleton/table/tabs/toast only). Rendered as a horizontal
//    row of real, clickable agent-name chips instead — same "switch whose
//    canvas is showing" function, a different control shape.
// 2. Version picker — same reasoning: a row of real version pills (newest
//    first) rather than the mockup's dropdown.
// 3. Kebab menu (⋯) — the web version's dropdown holds exactly one item
//    (重新整理); folded into the same refresh button the version row already
//    needs, rather than building a menu primitive for a single action.
// 4. Pan/zoom — `screens::panzoom::PanZoomState` (see that module's own doc
//    comment for why this is re-layout zoom, not a GPU transform) lets the
//    content card be dragged and wheel-zoomed within the frame, per this
//    WP's brief ("pan/zoom 用 spike_t7_panzoom.rs 配方").

use gpui::{div, prelude::*, px, Context, Div, Global, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ScrollDelta, ScrollWheelEvent, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::api;
use crate::i18n::{self, Locale};
use crate::mds_gpui::{button, empty_state, skeleton, ButtonVariant};
use crate::rpc::CallError;
use crate::screens::agents_data::{parse_agents_list, AgentListItem};
use crate::screens::dashboard::Loadable;
use crate::screens::goals::relative_time;
use crate::screens::panzoom::{wheel_zoom_factor, PanZoomState};
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct CanvasVersionMeta {
    pub seq: i64,
    pub title: String,
    pub updated_at: String,
    pub bytes: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CanvasInfo {
    pub seq: i64,
    pub title: String,
    pub html_len: usize,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CanvasGetResult {
    pub canvas: Option<CanvasInfo>,
    pub history: Vec<CanvasVersionMeta>,
}

pub fn parse_canvas_get(v: &Value) -> CanvasGetResult {
    let canvas = v.get("canvas").filter(|c| !c.is_null()).map(|c| CanvasInfo {
        seq: c.get("seq").and_then(Value::as_i64).unwrap_or(0),
        title: c.get("title").and_then(Value::as_str).unwrap_or("").to_string(),
        // Only the length is kept — the raw HTML is never rendered (see this
        // module's header comment), so there is no reason to hold onto
        // (potentially up to 256KB of) sanitized markup in page state.
        html_len: c.get("html").and_then(Value::as_str).map(str::len).unwrap_or(0),
        updated_at: c.get("updated_at").and_then(Value::as_str).unwrap_or("").to_string(),
    });
    let history = v
        .get("history")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|h| {
                    Some(CanvasVersionMeta {
                        seq: h.get("seq").and_then(Value::as_i64)?,
                        title: h.get("title").and_then(Value::as_str).unwrap_or("").to_string(),
                        updated_at: h.get("updated_at").and_then(Value::as_str).unwrap_or("").to_string(),
                        bytes: h.get("bytes").and_then(Value::as_i64).unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    CanvasGetResult { canvas, history }
}

// ── Global state ───────────────────────────────────────────────────────

pub struct CanvasState {
    requested_agents: bool,
    pub agents: Loadable<Vec<AgentListItem>>,
    pub selected_agent: Option<String>,
    pub view_seq: Option<i64>,
    /// `(agent_id, view_seq)` the current `result` was fetched for — the
    /// fetch latch, mirrors `plans.rs::PlansState::detail_for`'s role.
    fetched_for: Option<(String, Option<i64>)>,
    pub result: Loadable<CanvasGetResult>,
    pub panzoom: PanZoomState,
}

impl CanvasState {
    fn new() -> Self {
        Self {
            requested_agents: false,
            agents: Loadable::Loading,
            selected_agent: None,
            view_seq: None,
            fetched_for: None,
            result: Loadable::Loading,
            panzoom: PanZoomState::default(),
        }
    }
}

impl Global for CanvasState {}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<CanvasState>() {
        cx.set_global(CanvasState::new());
    }
}

/// Which agent id is effectively showing — the explicit selection when it
/// still resolves to a live row, else the first row. Mirrors `plans.
/// rs::resolve_selected_id`'s exact shape.
pub fn resolve_effective_agent(explicit: &Option<String>, rows: &[AgentListItem]) -> Option<String> {
    if let Some(id) = explicit {
        if rows.iter().any(|r| &r.id == id) {
            return Some(id.clone());
        }
    }
    rows.first().map(|r| r.id.clone())
}

// ── Fetch orchestration ───────────────────────────────────────────────

fn maybe_fetch_agents(state: &RootView, cx: &mut Context<RootView>) {
    if cx.global::<CanvasState>().requested_agents {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<CanvasState>().requested_agents = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "agents.list", json!({}), |cx, result| {
        cx.global_mut::<CanvasState>().agents = result.map(|v| parse_agents_list(&v)).into();
    });
}

fn maybe_fetch_canvas(state: &RootView, cx: &mut Context<RootView>, agent_id: &str, seq: Option<i64>) {
    let key = (agent_id.to_string(), seq);
    if cx.global::<CanvasState>().fetched_for.as_ref() == Some(&key) {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    {
        let g = cx.global_mut::<CanvasState>();
        g.fetched_for = Some(key.clone());
        g.result = Loadable::Loading;
    }
    let tx = state.session_tx.clone();
    let mut params = json!({ "agent_id": agent_id });
    if let Some(s) = seq {
        params["seq"] = json!(s);
    }
    spawn_call(cx, tx, "canvas.get", params, move |cx, result| {
        // Stale response (selection changed again while in flight) dropped.
        if cx.global::<CanvasState>().fetched_for.as_ref() != Some(&key) {
            return;
        }
        cx.global_mut::<CanvasState>().result = result.map(|v| parse_canvas_get(&v)).into();
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
        CallError::Rejected(v) => v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()),
    }
}

// ── Floating controls ──────────────────────────────────────────────────

fn agent_chip(agent: &AgentListItem, selected: bool, cx: &mut Context<RootView>) -> Stateful<Div> {
    let id_for_click = agent.id.clone();
    let row_id: SharedString = format!("canvas-agent-{}", agent.id).into();
    div()
        .id(row_id)
        .px_2p5()
        .py_1()
        .rounded(px(theme::RADIUS_4XL))
        .cursor_pointer()
        .text_size(px(theme::TEXT_XS))
        .font_weight(if selected { gpui::FontWeight::SEMIBOLD } else { gpui::FontWeight::NORMAL })
        .bg(if selected { theme::alpha(theme::SURFACE, 1.0) } else { theme::alpha(theme::SURFACE, 0.0) })
        .text_color(if selected { theme::alpha(theme::BRAND, 1.0) } else { theme::alpha(theme::MUTED_FOREGROUND, 1.0) })
        .when(!selected, |el| el.hover(|s| s.bg(theme::alpha(theme::SURFACE_HOVER, 1.0))))
        .child(if agent.display_name.is_empty() { agent.id.clone() } else { agent.display_name.clone() })
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            let g = cx.global_mut::<CanvasState>();
            g.selected_agent = Some(id_for_click.clone());
            g.view_seq = None;
            cx.notify();
        }))
}

fn version_pill(v: &CanvasVersionMeta, is_current: bool, active: bool, locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    let seq = v.seq;
    let label = if is_current {
        i18n::t(locale, "canvasPage.version.current")
    } else {
        i18n::t1(locale, "canvasPage.version.label", "seq", &v.seq.to_string())
    };
    let row_id: SharedString = format!("canvas-version-{seq}").into();
    div()
        .id(row_id)
        .px_2p5()
        .py_1()
        .rounded(px(theme::RADIUS_4XL))
        .cursor_pointer()
        .text_size(px(theme::TEXT_XS))
        .bg(theme::alpha(theme::SURFACE, if active { 1.0 } else { 0.0 }))
        .text_color(if active { theme::alpha(theme::FOREGROUND, 1.0) } else { theme::alpha(theme::MUTED_FOREGROUND, 1.0) })
        .when(!active, |el| el.hover(|s| s.bg(theme::alpha(theme::SURFACE_HOVER, 1.0))))
        .child(label)
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            let g = cx.global_mut::<CanvasState>();
            g.view_seq = if is_current { None } else { Some(seq) };
            cx.notify();
        }))
}

fn refresh_button(cx: &mut Context<RootView>) -> Stateful<Div> {
    div()
        .id("canvas-refresh")
        .size(px(28.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(theme::RADIUS_LG))
        .cursor_pointer()
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::surface_shadow())
        .text_size(px(theme::TEXT_SM))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
        .child("↻")
        .on_click(cx.listener(|_this, _ev, _window, cx| {
            cx.global_mut::<CanvasState>().fetched_for = None;
            cx.notify();
        }))
}

// ── Content pane (pan/zoom-able) ───────────────────────────────────────

const VIEWPORT_HEIGHT: f32 = 520.0;
const CARD_BASE_WIDTH: f32 = 520.0;

fn content_card(agent_id: &str, info: &CanvasInfo, locale: Locale, zoom: f32) -> Div {
    let when = relative_time(locale, &info.updated_at, chrono::Utc::now());
    let dashboard_url: SharedString = format!("{}/canvas?agent={}", api::gateway_base_url(), agent_id).into();

    div()
        .w(px((CARD_BASE_WIDTH * zoom).max(220.0)))
        .flex()
        .flex_col()
        .gap_2p5()
        .p(px(20.0 * zoom.max(0.6)))
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::surface_shadow())
        .child(
            div()
                .text_size(px((theme::TEXT_BASE * zoom).clamp(10.0, 28.0)))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(if info.title.is_empty() { i18n::t(locale, "canvasPage.untitled") } else { info.title.clone().into() }),
        )
        .child(
            div()
                .text_size(px((theme::TEXT_XS * zoom).clamp(9.0, 16.0)))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::t1(locale, "canvasPage.updatedAt", "time", &when)),
        )
        .child(
            div()
                .mt_2()
                .p_3()
                .rounded(px(theme::RADIUS_LG))
                .bg(theme::alpha(theme::MUTED, 0.5))
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::t(locale, "canvasPage.htmlPlaceholder")),
        )
        .child(button(
            "canvas-open-browser",
            i18n::t(locale, "canvasPage.openInBrowser"),
            ButtonVariant::Secondary,
            false,
            None,
            move |_ev, _window, app| app.open_url(&dashboard_url),
        ))
}

fn pan_zoom_frame(inner: Div, cx: &mut Context<RootView>) -> Stateful<Div> {
    let panzoom = cx.global::<CanvasState>().panzoom;
    div()
        .id("canvas-viewport")
        .flex_1()
        .h(px(VIEWPORT_HEIGHT))
        .overflow_hidden()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::PAGE_CANVAS, 1.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor(if panzoom.dragging { gpui::CursorStyle::ClosedHand } else { gpui::CursorStyle::OpenHand })
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|_this, ev: &MouseDownEvent, _window, cx| {
                let g = cx.global_mut::<CanvasState>();
                g.panzoom.dragging = true;
                g.panzoom.drag_origin = ev.position;
                g.panzoom.pan_at_drag_start = g.panzoom.pan;
                cx.notify();
            }),
        )
        .on_mouse_move(cx.listener(|_this, ev: &MouseMoveEvent, _window, cx| {
            let g = cx.global_mut::<CanvasState>();
            if !g.panzoom.dragging {
                return;
            }
            let delta = ev.position - g.panzoom.drag_origin;
            g.panzoom.pan = g.panzoom.pan_at_drag_start + delta;
            cx.notify();
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|_this, _ev: &MouseUpEvent, _window, cx| {
                cx.global_mut::<CanvasState>().panzoom.dragging = false;
                cx.notify();
            }),
        )
        .on_scroll_wheel(cx.listener(|_this, ev: &ScrollWheelEvent, _window, cx| {
            let delta_y: f32 = match ev.delta {
                ScrollDelta::Pixels(p) => f32::from(p.y),
                ScrollDelta::Lines(l) => l.y * 20.0,
            };
            let g = cx.global_mut::<CanvasState>();
            g.panzoom.zoom = (g.panzoom.zoom * wheel_zoom_factor(delta_y)).clamp(0.5, 2.0);
            cx.notify();
        }))
        .child(
            div()
                .absolute()
                .left(px(200.0) + panzoom.pan.x)
                .top(px(90.0) + panzoom.pan.y)
                .child(inner),
        )
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch_agents(state, cx);

    let locale = state.locale;

    if state.ws_state != WsConnState::Authenticated {
        return div()
            .id("canvas-page")
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(empty_state(
                "🔌",
                i18n::t(locale, "native.home.connError.title"),
                Some(i18n::t(locale, "native.home.connError.desc")),
                None::<Div>,
            ));
    }

    let agents_loadable = cx.global::<CanvasState>().agents.clone();

    let body: Div = match &agents_loadable {
        Loadable::Loading => div().flex_1().flex().items_center().justify_center().child(skeleton(px(320.), px(220.))),
        Loadable::Failed(e) => div().flex_1().flex().items_center().justify_center().child(empty_state(
            "⚠️",
            i18n::t1(locale, "canvasPage.loadError", "message", e),
            None,
            None::<Div>,
        )),
        Loadable::Ready(rows) if rows.is_empty() => div().flex_1().flex().items_center().justify_center().child(
            empty_state("🖼️", i18n::t(locale, "canvasPage.noAgents"), None, None::<Div>),
        ),
        Loadable::Ready(rows) => {
            let effective = resolve_effective_agent(&cx.global::<CanvasState>().selected_agent, rows);
            let Some(agent_id) = effective else {
                return div().id("canvas-page").size_full();
            };
            let view_seq = cx.global::<CanvasState>().view_seq;
            maybe_fetch_canvas(state, cx, &agent_id, view_seq);

            let mut chip_row = div().flex().flex_wrap().gap_1();
            for a in rows {
                chip_row = chip_row.child(agent_chip(a, a.id == agent_id, cx));
            }

            let result = cx.global::<CanvasState>().result.clone();
            let history_row = if let Loadable::Ready(r) = &result {
                if r.history.is_empty() {
                    None
                } else {
                    // Newest first (server-guaranteed) — the first row is
                    // "current"; `view_seq` (`None` ⇒ current) decides which
                    // pill is highlighted active.
                    let current_seq = r.history.first().map(|h| h.seq);
                    let active_seq = view_seq.or(current_seq);
                    let mut row = div().flex().flex_wrap().gap_1();
                    for v in &r.history {
                        row = row.child(version_pill(v, Some(v.seq) == current_seq, Some(v.seq) == active_seq, locale, cx));
                    }
                    Some(row)
                }
            } else {
                None
            };

            let controls = div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .flex_wrap()
                .p_1()
                .rounded(px(theme::RADIUS_LG))
                .bg(theme::alpha(theme::SURFACE, 0.9))
                .border_1()
                .border_color(theme::surface_border())
                .shadow(theme::surface_shadow())
                .child(chip_row)
                .child(div().flex().items_center().gap_2().children(history_row).child(refresh_button(cx)));

            let zoom = cx.global::<CanvasState>().panzoom.zoom;
            let content: Div = match &result {
                Loadable::Loading => div().flex_1().flex().items_center().justify_center().child(skeleton(px(320.), px(220.))),
                Loadable::Failed(e) => div().flex_1().flex().items_center().justify_center().child(empty_state(
                    "⚠️",
                    i18n::t1(locale, "canvasPage.loadError", "message", e),
                    None,
                    None::<Div>,
                )),
                Loadable::Ready(r) => match &r.canvas {
                    Some(info) if info.html_len > 0 => {
                        div().flex_1().child(pan_zoom_frame(content_card(&agent_id, info, locale, zoom), cx))
                    }
                    _ => div().flex_1().flex().items_center().justify_center().child(empty_state(
                        "🖼️",
                        i18n::t(locale, "canvasPage.empty"),
                        Some(i18n::t(locale, "canvasPage.empty.hint")),
                        None::<Div>,
                    )),
                },
            };

            div().flex_1().flex().flex_col().gap_2().child(controls).child(content)
        }
    };

    div().id("canvas-page").size_full().flex().flex_col().gap_2().p_4().child(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_canvas_get_reads_canvas_and_history() {
        let v = json!({
            "agent_id": "dudu",
            "canvas": { "seq": 4, "agent_id": "dudu", "title": "客服月報", "html": "<div>report</div>", "updated_at": "2026-08-21T10:00:00Z" },
            "history": [
                { "seq": 4, "title": "客服月報", "updated_at": "2026-08-21T10:00:00Z", "bytes": 512 },
                { "seq": 3, "title": "客服月報 v3", "updated_at": "2026-08-20T10:00:00Z", "bytes": 480 },
            ],
        });
        let r = parse_canvas_get(&v);
        let info = r.canvas.expect("canvas present");
        assert_eq!(info.seq, 4);
        assert_eq!(info.title, "客服月報");
        assert!(info.html_len > 0);
        assert_eq!(r.history.len(), 2);
        assert_eq!(r.history[0].seq, 4);
    }

    #[test]
    fn parse_canvas_get_null_canvas_is_none() {
        let v = json!({ "agent_id": "dudu", "canvas": null, "history": [] });
        let r = parse_canvas_get(&v);
        assert!(r.canvas.is_none());
        assert!(r.history.is_empty());
    }

    #[test]
    fn parse_canvas_get_empty_html_still_parses_as_present() {
        // An agent that cleared its canvas sends an empty-string `html`
        // (`canvas.rs`'s own gateway-side tombstone convention) — the
        // struct still parses; `render`'s own `html_len > 0` check is what
        // decides whether that reads as "empty" on screen, not this parser.
        let v = json!({ "agent_id": "dudu", "canvas": { "seq": 5, "agent_id": "dudu", "title": "", "html": "", "updated_at": "t" }, "history": [] });
        let r = parse_canvas_get(&v);
        assert_eq!(r.canvas.unwrap().html_len, 0);
    }

    #[test]
    fn resolve_effective_agent_falls_back_to_first_row() {
        let rows = vec![AgentListItem {
            id: "a".into(),
            display_name: "A".into(),
            role: "".into(),
            department: "".into(),
            status: "active".into(),
            icon: None,
        }];
        assert_eq!(resolve_effective_agent(&None, &rows), Some("a".to_string()));
        assert_eq!(resolve_effective_agent(&Some("missing".to_string()), &rows), Some("a".to_string()));
        assert_eq!(resolve_effective_agent(&Some("a".to_string()), &rows), Some("a".to_string()));
        assert_eq!(resolve_effective_agent(&None, &[]), None);
    }
}
