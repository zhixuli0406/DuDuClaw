// WP-gpui-spike-T7 (2026-08-21) — Chromium-risk-page feasibility spike.
//
// `commercial/docs/TODO-native-gui-page-migration-2026-08.md`'s D10/"⚠️
// Chromium 級風險頁" discipline requires a ≤1-day spike proving "gpui 能不能
// 畫" BEFORE /timeline, /org, /canvas, /world, /forks are implemented for
// real — if a primitive can't be made to work, the discipline is to stop and
// report back for a re-plan, not silently ship a degraded page or (worse)
// sneak in a webview (this project is D2 "all-native", no embedded browser
// anywhere). This file (plus its two siblings `spike_t7_timeline.rs` /
// `spike_t7_panzoom.rs`) is that spike, covering the FOUR shared primitives
// those five pages all need:
//   1. edges/connectors (straight / polyline / bezier, ~50 on screen) — this
//      file, `render_edges_section`. Feeds /org (tree lines), /world
//      (relationship lines), /forks (comparison lines).
//   2. a horizontally-scrolling hour-grid timeline lane with colored blocks
//      — `spike_t7_timeline.rs`. Feeds /timeline.
//   3. a pan/zoom-able node group (drag to pan, wheel to zoom 0.5x–2x, text
//      labels re-scale) — `spike_t7_panzoom.rs`. Feeds /canvas, /world,
//      /org (whole-tree view).
//   4. the SAME two primitives above, stress-tested at 500 items via the
//      "壓測：500 量級" toggle in this file's header — no separate section,
//      per the task brief's "把節點/色塊拉到 500 個，目視是否順" wording
//      (it's a parameter sweep on primitives 2+3, not a fifth primitive).
//
// NOT a real product page: no nav.rs entry, unreachable through normal
// navigation. Only reachable via `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=spike_t7`
// (see `main.rs`'s debug-page boot override) landing on `screens::shell`'s
// `active_id == "spike_t7"` branch. Deletable wholesale once the spike
// verdict is filed — every primitive here is a standalone `canvas()` paint
// closure, not wired to any real RPC/data model.
//
// Painting technique precedent: `ime_input/element.rs` (custom `Element`
// with `prepaint`/`paint`, `ShapedLine::paint`, `PaintQuad`) and the vendored
// gpui example `crates/gpui/examples/painting.rs` (`PathBuilder` — confirmed
// to support `curve_to` (quadratic) and `cubic_bezier_to`, `arc_to`, dashed
// strokes, linear gradients — all read directly from the local gpui git
// checkout at the pinned rev, NOT assumed from memory). This spike uses the
// lower-level `canvas(prepaint, paint)` element (`gpui::canvas`) rather than
// a full custom `Element` impl for each primitive — cheaper to write for a
// throwaway spike, and `Canvas<T>` already implements `Styled` so normal
// `.w()/.h()/.size_full()` layout sizing works on it directly.
//
// Coordinate system note (verified against `painting.rs`, not assumed):
// `paint`'s `bounds: Bounds<Pixels>` argument is the canvas element's ACTUAL
// on-screen (window-space) rectangle — the same space `MouseMoveEvent::
// position` and every `PaintQuad`/`Path` coordinate live in. Every primitive
// below builds shapes in a small local coordinate space (content
// starting at (0,0)) and adds `bounds.origin` (and, for the pan/zoom group,
// the current pan offset scaled appropriately) before painting, exactly like
// `ime_input/element.rs`'s `row_top = bounds.top() + line_height * row_idx`.

use gpui::{div, prelude::*, px, Context, Div, ScrollHandle, Stateful};

use crate::mds_gpui::button;
use crate::screens::{spike_t7_panzoom, spike_t7_timeline};
use crate::theme;
use crate::RootView;

/// Debug-spike-only state (see this file's module doc comment for why this
/// whole feature is throwaway). Lives directly on `RootView` — same
/// "plain struct field, not an `Entity<T>`" shape as `RootView::chat`
/// (`screens/chat.rs`) — because ordinary `cx.listener(|this: &mut
/// RootView, ...|)` mutation already reaches it with no extra indirection.
pub struct SpikeT7State {
    /// Current pan offset for the group-3 canvas, in on-screen pixels
    /// (accumulated drag delta).
    pub pan: gpui::Point<gpui::Pixels>,
    /// Current zoom factor for the group-3 canvas, clamped to the task
    /// brief's stated 0.5x–2x range.
    pub zoom: f32,
    /// True while a left-mouse-button drag is in progress over the group-3
    /// canvas.
    pub dragging: bool,
    /// Mouse position (window space) at the start of the current drag —
    /// `pan` becomes `pan_at_drag_start + (current_position -
    /// drag_origin)`, the standard "anchor + delta" drag recipe (same shape
    /// `ime_input/input_state.rs`'s `on_mouse_move` uses for text selection).
    pub drag_origin: gpui::Point<gpui::Pixels>,
    pub pan_at_drag_start: gpui::Point<gpui::Pixels>,
    /// Primitive 4 (量級壓測): false = baseline count (50 nodes / 100
    /// timeline blocks), true = the brief's 500-item stress count. One
    /// shared toggle drives both group 2 and group 3 so a single button
    /// exercises the "does it still track the mouse smoothly at 500"
    /// question on the two volume-sensitive primitives.
    pub stress_mode: bool,
    /// Scroll offset for the whole spike page (four sections stacked
    /// vertically can exceed the content shell's fixed height — see
    /// `screens/shell.rs`'s `content_shell` `.overflow_hidden()` — so this
    /// page needs its own scroll container, same pattern as `screens/
    /// chat.rs`'s `ChatState::scroll_handle`).
    pub page_scroll: ScrollHandle,
    /// Horizontal scroll offset for the group-2 timeline lane
    /// (`spike_t7_timeline.rs`). A separate persistent handle (not a fresh
    /// `ScrollHandle::new()` per render) so clicking the stress toggle or
    /// dragging the group-3 canvas doesn't silently reset the timeline's
    /// scroll position on the next re-render.
    pub timeline_scroll: ScrollHandle,
}

impl Default for SpikeT7State {
    fn default() -> Self {
        Self {
            pan: gpui::point(px(0.), px(0.)),
            zoom: 1.0,
            dragging: false,
            drag_origin: gpui::point(px(0.), px(0.)),
            pan_at_drag_start: gpui::point(px(0.), px(0.)),
            stress_mode: false,
            page_scroll: ScrollHandle::new(),
            timeline_scroll: ScrollHandle::new(),
        }
    }
}

/// Deterministic pseudo-random `[0, 1)` float from an integer seed —
/// xorshift64*, chosen ONLY because this crate has no `rand` dependency
/// (see `Cargo.toml`) and a throwaway spike's node/color scatter doesn't
/// justify adding one. Deterministic on purpose: re-running the spike
/// produces the identical layout, which matters for "did it get slower"
/// comparisons across the 50→500 stress toggle.
pub(super) fn pseudo_rand(seed: u64) -> f32 {
    let mut x = seed ^ 0x9E37_79B9_7F4A_7C15;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    ((x >> 11) as f64 / (1u64 << 53) as f64) as f32
}

/// One of five MDS chart hues, cycling by index — gives the node/block
/// scatter visible variety without inventing a new palette.
pub(super) fn chart_color(index: usize) -> u32 {
    const CHART: [u32; 5] = [theme::CHART_1, theme::CHART_2, theme::CHART_3, theme::CHART_4, theme::CHART_5];
    CHART[index % CHART.len()]
}

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    let spike = &state.spike_t7;
    let stress_mode = spike.stress_mode;

    let toggle_label = if stress_mode { "壓測：500 量級（點擊回到基準）" } else { "壓測：切到 500 量級" };

    let header = div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(theme::TEXT_XL))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child("WP-gpui-spike-T7 — Chromium 級風險頁原語 spike"),
        )
        .child(
            div()
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(
                    "debug-only；不掛正式導覽。三組畫布：① 連接線 ② 時間軸格 ③ pan/zoom 節點群，\
                     右上角切換 50↔500 量級檢驗卡頓門檻。",
                ),
        )
        .child(
            button::button(
                "spike-t7-stress-toggle",
                toggle_label,
                if stress_mode { button::ButtonVariant::Primary } else { button::ButtonVariant::Secondary },
                false,
                None,
                cx.listener(|this: &mut RootView, _ev, _window, cx| {
                    this.spike_t7.stress_mode = !this.spike_t7.stress_mode;
                    cx.notify();
                }),
            ),
        );

    div()
        .id("spike-t7-root")
        .size_full()
        .track_scroll(&spike.page_scroll)
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_4()
        .child(header)
        .child(section_frame("① 連接線／邊 — 直線＋折線＋貝茲曲線＋端點圓（~50 條）", render_edges_section()))
        .child(section_frame(
            "② 軌道時間格 — 橫向小時格線＋三軌道色塊＋橫向捲動",
            spike_t7_timeline::render(stress_mode, &spike.timeline_scroll),
        ))
        .child(section_frame(
            "③ pan/zoom 節點群 — 拖曳平移＋滾輪縮放（0.5x–2x）＋文字隨縮放",
            spike_t7_panzoom::render(state, cx),
        ))
}

fn section_frame(title: &'static str, content: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .child(
            div()
                .text_size(px(theme::TEXT_SM))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(title),
        )
        .child(content)
}

/// Primitive 1 — no interactivity, no stress-mode scaling (the brief's
/// "~50 條同屏" is already the ceiling this primitive is asked to prove, not
/// a variable). Static per render since nothing here reads mutable state.
fn render_edges_section() -> Div {
    const WIDTH: f32 = 860.0;
    const HEIGHT: f32 = 220.0;
    const EDGE_COUNT: usize = 50;

    div().w(px(WIDTH)).h(px(HEIGHT)).rounded(px(theme::RADIUS_MD)).bg(theme::alpha(theme::PAGE_CANVAS, 1.0)).child(
        gpui::canvas(
            move |_bounds, _window, _cx| {},
            move |bounds, _prepaint, window, _cx| {
                let left_x = 40.0_f32;
                let right_x = WIDTH - 40.0;
                let usable_h = HEIGHT - 40.0;

                for i in 0..EDGE_COUNT {
                    let seed = i as u64;
                    let start_y = 20.0 + pseudo_rand(seed * 2 + 1) * usable_h;
                    let end_y = 20.0 + pseudo_rand(seed * 2 + 2) * usable_h;
                    let start = bounds.origin + gpui::point(px(left_x), px(start_y));
                    let end = bounds.origin + gpui::point(px(right_x), px(end_y));
                    let color = theme::alpha(chart_color(i), 0.75);

                    let mut builder = gpui::PathBuilder::stroke(px(1.5));
                    builder.move_to(start);
                    match i % 3 {
                        // Straight line — the simplest connector, e.g. a
                        // direct /world relationship edge.
                        0 => builder.line_to(end),
                        // Polyline with a right-angle elbow at the
                        // horizontal midpoint — the classic org-chart
                        // connector shape /org needs.
                        1 => {
                            let mid_x = (left_x + right_x) / 2.0;
                            builder.line_to(bounds.origin + gpui::point(px(mid_x), px(start_y)));
                            builder.line_to(bounds.origin + gpui::point(px(mid_x), px(end_y)));
                            builder.line_to(end);
                        }
                        // Cubic bezier with horizontally-offset control
                        // points — a smooth flow-diagram edge, e.g. /forks
                        // comparing two branches.
                        _ => {
                            let ctrl_dx = px((right_x - left_x) * 0.35);
                            builder.cubic_bezier_to(end, start + gpui::point(ctrl_dx, px(0.)), end - gpui::point(ctrl_dx, px(0.)));
                        }
                    }
                    if let Ok(path) = builder.build() {
                        window.paint_path(path, color);
                    }

                    // Endpoint markers: a 6px rounded quad at each end
                    // (corner radius == half the side length renders as a
                    // circle — same trick used throughout this crate's own
                    // MDS components, e.g. `mds_gpui/badge.rs`'s dot).
                    for center in [start, end] {
                        let r = px(3.0);
                        let dot_bounds =
                            gpui::Bounds::new(gpui::point(center.x - r, center.y - r), gpui::size(r * 2., r * 2.));
                        window.paint_quad(gpui::quad(
                            dot_bounds,
                            r,
                            theme::alpha(chart_color(i), 1.0),
                            px(0.),
                            gpui::transparent_black(),
                            gpui::BorderStyle::default(),
                        ));
                    }
                }
            },
        )
        .size_full(),
    )
}
