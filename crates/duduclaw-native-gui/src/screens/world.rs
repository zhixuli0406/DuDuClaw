// WP-S5b3-I (2026-08-21) — 世界 (`World.dc.html`, a B12 variant). A flat
// floor-plan of every AI staff member as a token — the native counterpart of
// the web dashboard's PixiJS isometric `/world` stage (`web/src/pages/
// WorldPage.tsx` + `web/src/components/world/WorldStage.tsx`).
//
// ── RPC shape (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, not guessed) ─────────────────────────────────────────────
//   `agents.list {}` → `handle_agents_list_filtered` (~L5396), parsed via
//     the pre-existing `screens::agents_data::{AgentListItem,
//     parse_agents_list}` — the SAME shape `web/src/pages/WorldPage.tsx`
//     itself is built on (it calls `useAgentsStore().fetchAgents()`, which
//     is this exact RPC; there is no separate "world state" RPC anywhere in
//     the gateway — grep-verified, no `world.` method prefix exists at all).
//     `status` is `"active" | "paused" | "terminated"` — the same three
//     values `web/src/components/home/WorldStagePlaceholder.tsx`'s own
//     `WorldStageAgent::status` type documents.
//
// ── Canvas fidelity deviations (documented, not silent) ───────────────────
// 1. Per-token speech-bubble status text ("整理記憶中…"/"回覆中…"/"等你拍板"/
//    "待命中" in the mockup) has NO backing field anywhere in `agents.list`'s
//    payload (grep-verified) — same "no real data source" judgment call
//    `inbox.rs`'s own header comment documents for the mockup's 提及 filter.
//    Tokens here show only what the RPC actually carries: name + the same
//    status-dot overlay `screens::agents::avatar_with_status` already draws
//    on the "AI 員工" list (active=brand / paused=warning / terminated=
//    destructive), reused directly rather than a bespoke token style — no
//    fabricated bubble text.
// 2. Desks/meeting table/plants are static decoration (fixed positions, no
//    backing data) — same "decorative chrome, not data" labeling `widgets.
//    rs`'s thumbnail placeholder already establishes for this crate.
// 3. Scene switcher (辦公室/小鎮/休息室) is real, client-local UI state (like
//    the web `WorldStage`'s own `readSceneKey`/`writeSceneChoice`, which is
//    `localStorage`-only, not server state either) — it only tags which
//    pill is highlighted here; a full per-scene backdrop repaint is out of
//    this pass's scope (this page's floor plan does not change per scene).
// 4. Token layout is a deterministic scatter from each agent's own id (a
//    stable FNV-1a-style hash, not `spike_t7::pseudo_rand` — this is a real
//    product page, not the debug spike, so it gets its own tiny hash rather
//    than depending on a module documented as "NOT a standalone page").
// 5. Pan/zoom + "回正視角" (reset view) — `screens::panzoom::PanZoomState`,
//    same recipe/rationale `canvas.rs`'s own header comment documents.

use gpui::{div, prelude::*, px, Context, Div, Global, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ScrollDelta, ScrollWheelEvent, SharedString, Stateful};
use serde_json::json;
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::empty_state;
use crate::rpc::CallError;
use crate::screens::agents_data::{parse_agents_list, AgentListItem};
use crate::screens::dashboard::Loadable;
use crate::screens::panzoom::{wheel_zoom_factor, PanZoomState};
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

// ── Scene switcher (client-local, see this module's header comment §3) ──

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneKind {
    Office,
    Town,
    Lounge,
}

impl SceneKind {
    const ALL: [SceneKind; 3] = [SceneKind::Office, SceneKind::Town, SceneKind::Lounge];

    fn label_key(self) -> &'static str {
        match self {
            SceneKind::Office => "world.scene.office",
            SceneKind::Town => "world.scene.town",
            SceneKind::Lounge => "world.scene.lounge",
        }
    }
}

// ── Deterministic token layout ────────────────────────────────────────

/// A tiny FNV-1a-style hash — deterministic, no external crate, good enough
/// for scattering a few dozen tokens across a fixed floor (not a
/// cryptographic or statistical requirement). Returns `[0, 1)`.
fn pseudo_rand(seed: &str, salt: u64) -> f32 {
    let mut h: u64 = 0xcbf29ce484222325 ^ salt;
    for b in seed.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    (h % 10_000) as f32 / 10_000.0
}

// ── Global state ───────────────────────────────────────────────────────

pub struct WorldState {
    requested: bool,
    pub agents: Loadable<Vec<AgentListItem>>,
    pub scene: SceneKind,
    pub panzoom: PanZoomState,
}

impl WorldState {
    fn new() -> Self {
        Self { requested: false, agents: Loadable::Loading, scene: SceneKind::Office, panzoom: PanZoomState::default() }
    }
}

impl Global for WorldState {}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<WorldState>() {
        cx.set_global(WorldState::new());
    }
}

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if cx.global::<WorldState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<WorldState>().requested = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "agents.list", json!({}), |cx, result| {
        cx.global_mut::<WorldState>().agents = result.map(|v| parse_agents_list(&v)).into();
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

// ── Floating controls ──────────────────────────────────────────────────

fn scene_pill(scene: SceneKind, active: bool, locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    let row_id: SharedString = format!("world-scene-{}", scene.label_key()).into();
    div()
        .id(row_id)
        .px_3()
        .py_1p5()
        .rounded(px(theme::RADIUS_4XL))
        .cursor_pointer()
        .text_size(px(theme::TEXT_XS))
        .font_weight(if active { gpui::FontWeight::SEMIBOLD } else { gpui::FontWeight::NORMAL })
        .bg(if active { theme::alpha(theme::BRAND, 1.0) } else { theme::alpha(theme::SURFACE, 0.0) })
        .text_color(if active { theme::alpha(theme::BRAND_FOREGROUND, 1.0) } else { theme::alpha(theme::MUTED_FOREGROUND, 1.0) })
        .child(i18n::t(locale, scene.label_key()))
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.global_mut::<WorldState>().scene = scene;
            cx.notify();
        }))
}

fn icon_button(id: &'static str, glyph: &'static str, on_click: impl Fn(&mut Context<RootView>) + 'static, cx: &mut Context<RootView>) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(30.))
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
        .child(glyph)
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            on_click(cx);
            cx.notify();
        }))
}

// ── Floor plan (pan/zoom-able) ─────────────────────────────────────────

const VIEWPORT_HEIGHT: f32 = 460.0;
const FLOOR_WIDTH: f32 = 780.0;
const FLOOR_HEIGHT: f32 = 360.0;
const TOKEN_SIZE: f32 = 36.0;
/// Web's own `BUST_CAP` (`WorldStageStatic.tsx`) — the same "show N, then a
/// +overflow chip" cap, applied here to the floor scatter instead of a
/// bottom row.
const TOKEN_CAP: usize = 24;

fn desk(left: f32, top: f32, pan: gpui::Point<gpui::Pixels>, zoom: f32) -> Div {
    div()
        .absolute()
        .left(px(left * zoom) + pan.x)
        .top(px(top * zoom) + pan.y)
        .w(px(46.0 * zoom))
        .h(px(28.0 * zoom))
        .rounded(px(theme::RADIUS_SM))
        .bg(theme::alpha(0xe7ded0, 0.9))
        .border_1()
        .border_color(theme::alpha(0xd9cdb8, 1.0))
}

fn agent_token(agent: &AgentListItem, index: usize, pan: gpui::Point<gpui::Pixels>, zoom: f32) -> Stateful<Div> {
    let seed = agent.id.as_str();
    let local_x = pseudo_rand(seed, index as u64 * 7 + 1) * (FLOOR_WIDTH - TOKEN_SIZE);
    let local_y = pseudo_rand(seed, index as u64 * 7 + 2) * (FLOOR_HEIGHT - TOKEN_SIZE);
    let token_id: SharedString = format!("world-token-{}", agent.id).into();

    div()
        .id(token_id)
        .absolute()
        .left(px(local_x * zoom) + pan.x)
        .top(px(local_y * zoom) + pan.y)
        .flex()
        .flex_col()
        .items_center()
        .gap_1()
        // Same circular avatar + status-dot overlay the "AI 員工" list already
        // draws (`crate::screens::agents::avatar_with_status`) — reused
        // rather than a bespoke square token, so a staff member's status hue
        // (active=brand / paused=warning / terminated=destructive, see
        // `agents::status_dot_color`) reads identically on both pages.
        .child(crate::screens::agents::avatar_with_status(
            &agent.id,
            &agent.display_name,
            agent.icon.as_deref(),
            &agent.status,
            px(TOKEN_SIZE * zoom),
        ))
        .child(
            div()
                .px_1p5()
                .py_0p5()
                .rounded(px(theme::RADIUS_4XL))
                .bg(theme::alpha(theme::SURFACE, 0.85))
                .text_size(px((theme::TEXT_XS * zoom).clamp(8.0, 14.0)))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(if agent.display_name.is_empty() { agent.id.clone() } else { agent.display_name.clone() }),
        )
}

fn floor_plan(agents: &[AgentListItem], pan: gpui::Point<gpui::Pixels>, zoom: f32) -> Div {
    let mut floor = div().relative().w(px(FLOOR_WIDTH * zoom)).h(px(FLOOR_HEIGHT * zoom));
    // A handful of static desks — decorative, see this module's header
    // comment §2.
    for (lx, ty) in [(60.0, 60.0), (200.0, 40.0), (500.0, 70.0), (640.0, 50.0), (120.0, 260.0), (560.0, 250.0)] {
        floor = floor.child(desk(lx, ty, pan, zoom));
    }
    for (i, a) in agents.iter().take(TOKEN_CAP).enumerate() {
        floor = floor.child(agent_token(a, i, pan, zoom));
    }
    floor
}

fn pan_zoom_viewport(agents: &[AgentListItem], cx: &mut Context<RootView>) -> Stateful<Div> {
    let panzoom = cx.global::<WorldState>().panzoom;
    let floor = floor_plan(agents, panzoom.pan, panzoom.zoom);

    div()
        .id("world-viewport")
        .flex_1()
        .h(px(VIEWPORT_HEIGHT))
        .overflow_hidden()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(0xf6f7f9, 1.0))
        .relative()
        .cursor(if panzoom.dragging { gpui::CursorStyle::ClosedHand } else { gpui::CursorStyle::OpenHand })
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|_this, ev: &MouseDownEvent, _window, cx| {
                let g = cx.global_mut::<WorldState>();
                g.panzoom.dragging = true;
                g.panzoom.drag_origin = ev.position;
                g.panzoom.pan_at_drag_start = g.panzoom.pan;
                cx.notify();
            }),
        )
        .on_mouse_move(cx.listener(|_this, ev: &MouseMoveEvent, _window, cx| {
            let g = cx.global_mut::<WorldState>();
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
                cx.global_mut::<WorldState>().panzoom.dragging = false;
                cx.notify();
            }),
        )
        .on_scroll_wheel(cx.listener(|_this, ev: &ScrollWheelEvent, _window, cx| {
            let delta_y: f32 = match ev.delta {
                ScrollDelta::Pixels(p) => f32::from(p.y),
                ScrollDelta::Lines(l) => l.y * 20.0,
            };
            let g = cx.global_mut::<WorldState>();
            g.panzoom.zoom = (g.panzoom.zoom * wheel_zoom_factor(delta_y)).clamp(0.5, 2.0);
            cx.notify();
        }))
        .child(floor)
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch(state, cx);

    let locale = state.locale;

    if state.ws_state != WsConnState::Authenticated {
        return div()
            .id("world-page")
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

    let agents_loadable = cx.global::<WorldState>().agents.clone();
    let scene = cx.global::<WorldState>().scene;

    let mut scene_row = div().flex().items_center().gap_1().p_1().rounded(px(theme::RADIUS_4XL)).bg(theme::alpha(theme::SURFACE, 0.92)).shadow(theme::surface_shadow());
    for s in SceneKind::ALL {
        scene_row = scene_row.child(scene_pill(s, s == scene, locale, cx));
    }

    let (online_count, body): (usize, Div) = match &agents_loadable {
        Loadable::Loading => (0, div().flex_1().flex().items_center().justify_center().child(empty_state("⏳", i18n::t(locale, "world.loading"), None, None::<Div>))),
        Loadable::Failed(e) => (0, div().flex_1().flex().items_center().justify_center().child(empty_state("⚠️", i18n::t1(locale, "world.loadError", "message", e), None, None::<Div>))),
        Loadable::Ready(rows) if rows.is_empty() => (0, div().flex_1().flex().items_center().justify_center().child(empty_state("🏢", i18n::t(locale, "world.empty"), None, None::<Div>))),
        Loadable::Ready(rows) => {
            let count = rows.iter().filter(|a| a.status == "active").count();
            (count, div().flex_1().child(pan_zoom_viewport(rows, cx)))
        }
    };

    let online_badge = div()
        .flex()
        .items_center()
        .gap_1p5()
        .px_2p5()
        .py_1()
        .rounded(px(theme::RADIUS_4XL))
        .bg(theme::alpha(theme::SURFACE, 0.92))
        .shadow(theme::surface_shadow())
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(div().size(px(7.)).rounded_full().bg(theme::alpha(theme::SUCCESS, 1.0)))
        .child(i18n::t1(locale, "world.online", "count", &online_count.to_string()));

    let reset_view = icon_button("world-reset-view", "⤢", |cx| cx.global_mut::<WorldState>().panzoom.reset(), cx);
    let refresh = icon_button(
        "world-refresh",
        "↻",
        |cx| {
            let g = cx.global_mut::<WorldState>();
            g.requested = false;
            g.agents = Loadable::Loading;
        },
        cx,
    );

    let controls = div()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .flex_wrap()
        .child(online_badge)
        .child(scene_row)
        .child(div().flex().items_center().gap_2().child(reset_view).child(refresh));

    div().id("world-page").size_full().flex().flex_col().gap_2().p_4().child(controls).child(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pseudo_rand_is_deterministic_and_in_range() {
        let a = pseudo_rand("dudu", 7);
        let b = pseudo_rand("dudu", 7);
        assert_eq!(a, b);
        assert!((0.0..1.0).contains(&a));
    }

    #[test]
    fn pseudo_rand_differs_by_seed() {
        assert_ne!(pseudo_rand("dudu", 1), pseudo_rand("acai", 1));
    }
}
