// WP-gpui-spike-T7 — primitive 3: a pan/zoom-able node group. See
// `spike_t7.rs`'s module doc comment for the spike's overall scope; this
// file is one of its three sibling primitive modules, not a standalone page.
//
// Feeds /canvas (free-form board), /world (relationship map), and /org
// (whole-tree pan/zoom) — all three need "drag to pan, wheel to zoom, text
// stays legible at any zoom" as their base interaction, which is exactly
// what this primitive isolates.
//
// Interaction wiring is plain `Div` mouse/scroll listeners via
// `cx.listener` (`RootView::spike_t7` mutation), the SAME recipe
// `ime_input/input_state.rs` uses for drag-to-select ("anchor position at
// mouse-down, apply delta on every mouse-move while a flag is set, clear the
// flag on mouse-up"). Known, documented simplification carried over from
// that same precedent: `Div::on_mouse_move`/`on_mouse_up` only fire while
// the cursor is still hovering this element's hitbox (see `gpui`'s
// `elements/div.rs`), so a very fast drag that exits the canvas bounds
// before the next move event can leave `dragging` stuck `true` until the
// next click. A real (non-spike) implementation should register through
// the lower-level `Window::on_mouse_event` (unconditional, not hover-gated)
// instead — out of scope for a 1-day feasibility spike whose job is
// answering "can gpui paint+drag this at all", not shipping the polished
// version.
//
// Node painting is a single `canvas()` paint closure (not one `Div` per
// node) — deliberately mirrors how a real /canvas or /org page would need
// to be built for the 500-node case (hundreds of individually laid-out
// `Div`s would exercise gpui's flex/taffy layout solver instead of the
// paint fast path; this primitive is specifically testing the paint fast
// path, matching `ime_input/element.rs`'s existing custom-paint precedent).

use gpui::{
    div, prelude::*, px, Context, Div, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ScrollDelta,
    ScrollWheelEvent, Stateful,
};

use crate::screens::spike_t7::{chart_color, pseudo_rand};
use crate::theme;
use crate::RootView;

const VIEWPORT_WIDTH: f32 = 860.0;
const VIEWPORT_HEIGHT: f32 = 300.0;
/// The virtual "world" the nodes are scattered across — bigger than the
/// viewport so panning actually reveals off-screen content instead of just
/// jittering nodes that were already fully visible.
const WORLD_WIDTH: f32 = 1600.0;
const WORLD_HEIGHT: f32 = 1000.0;
const NODE_SIZE: f32 = 26.0;

const BASELINE_NODES: usize = 50;
const STRESS_NODES: usize = 500;

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    let spike = &state.spike_t7;
    let node_count = if spike.stress_mode { STRESS_NODES } else { BASELINE_NODES };
    let pan = spike.pan;
    let zoom = spike.zoom;

    div()
        .id("spike-t7-panzoom-area")
        .w(px(VIEWPORT_WIDTH))
        .h(px(VIEWPORT_HEIGHT))
        .overflow_hidden()
        .rounded(px(theme::RADIUS_MD))
        .bg(theme::alpha(theme::PAGE_CANVAS, 1.0))
        .cursor(if spike.dragging { gpui::CursorStyle::ClosedHand } else { gpui::CursorStyle::OpenHand })
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this: &mut RootView, ev: &MouseDownEvent, _window, cx| {
                this.spike_t7.dragging = true;
                this.spike_t7.drag_origin = ev.position;
                this.spike_t7.pan_at_drag_start = this.spike_t7.pan;
                cx.notify();
            }),
        )
        .on_mouse_move(cx.listener(|this: &mut RootView, ev: &MouseMoveEvent, _window, cx| {
            if !this.spike_t7.dragging {
                return;
            }
            let delta = ev.position - this.spike_t7.drag_origin;
            this.spike_t7.pan = this.spike_t7.pan_at_drag_start + delta;
            cx.notify();
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this: &mut RootView, _ev: &MouseUpEvent, _window, cx| {
                this.spike_t7.dragging = false;
                cx.notify();
            }),
        )
        .on_scroll_wheel(cx.listener(|this: &mut RootView, ev: &ScrollWheelEvent, _window, cx| {
            // Trackpad two-finger scroll and a real mouse wheel both land
            // here as `ScrollWheelEvent` on gpui/macOS; native pinch-to-zoom
            // is a SEPARATE `PinchEvent`/`on_pinch` this spike doesn't wire
            // (the task brief asks specifically for wheel-driven zoom).
            // `Lines` deltas (line-stepped wheels) are scaled up to feel
            // comparable to `Pixels` deltas (continuous trackpad scroll).
            let delta_y: f32 = match ev.delta {
                ScrollDelta::Pixels(p) => f32::from(p.y),
                ScrollDelta::Lines(l) => l.y * 20.0,
            };
            let factor = 1.0 + delta_y * 0.0015;
            this.spike_t7.zoom = (this.spike_t7.zoom * factor).clamp(0.5, 2.0);
            cx.notify();
        }))
        .child(
            gpui::canvas(
                move |_bounds, _window, _cx| {},
                move |bounds, _prepaint, window, cx| paint_nodes(bounds, pan, zoom, node_count, window, cx),
            )
            .size_full(),
        )
}

fn paint_nodes(
    bounds: gpui::Bounds<gpui::Pixels>,
    pan: gpui::Point<gpui::Pixels>,
    zoom: f32,
    node_count: usize,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) {
    let font = window.text_style().font();
    // Clamp the label size independently of the raw `zoom` factor — below
    // ~7px glyphs stop being legible at all (not a gpui limitation, just
    // typography), so a label fades out below that floor rather than
    // shrinking into unreadable mush.
    let label_size = px((11.0 * zoom).clamp(7.0, 20.0));
    let show_labels = 11.0 * zoom >= 7.0;

    for i in 0..node_count {
        let seed = i as u64;
        let local_x = pseudo_rand(seed * 5 + 1) * (WORLD_WIDTH - NODE_SIZE);
        let local_y = pseudo_rand(seed * 5 + 2) * (WORLD_HEIGHT - NODE_SIZE);

        let screen_origin = bounds.origin + pan + gpui::point(px(local_x), px(local_y)) * zoom;
        let node_size = px(NODE_SIZE * zoom);

        // Cheap off-screen culling — skip nodes entirely outside the
        // viewport rectangle before touching text shaping (the expensive
        // part). This is the one deliberate optimization this spike adds
        // beyond "paint everything naively"; see this file's module doc
        // comment for why brute-force-first is the point of the exercise.
        if screen_origin.x + node_size < bounds.origin.x
            || screen_origin.y + node_size < bounds.origin.y
            || screen_origin.x > bounds.origin.x + bounds.size.width
            || screen_origin.y > bounds.origin.y + bounds.size.height
        {
            continue;
        }

        let node_bounds = gpui::Bounds::new(screen_origin, gpui::size(node_size, node_size));
        window.paint_quad(gpui::quad(
            node_bounds,
            px(6.0 * zoom.max(0.5)),
            theme::alpha(chart_color(i), 0.88),
            px(1.0),
            theme::alpha(theme::SURFACE_BORDER, 1.0),
            gpui::BorderStyle::default(),
        ));

        if show_labels {
            let label: gpui::SharedString = format!("N{i}").into();
            let run = gpui::TextRun {
                len: label.len(),
                font: font.clone(),
                color: theme::alpha(theme::SURFACE_FOREGROUND, 1.0).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let shaped = window.text_system().shape_line(label, label_size, &[run], None);
            let text_origin = screen_origin + gpui::point(px(3.0 * zoom), px(3.0 * zoom));
            if let Err(e) = shaped.paint(text_origin, label_size, gpui::TextAlign::Left, None, window, cx) {
                eprintln!("[spike_t7_panzoom] node label paint failed: {e}");
            }
        }
    }
}
