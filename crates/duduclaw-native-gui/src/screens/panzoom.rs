// Shared pan/zoom primitive for `/canvas` and `/world` (WP-S5b3-I,
// 2026-08-21) — both pages need "drag to pan, wheel to zoom" over a small
// virtual workspace (a pushed report card; an office floor plan). Recipe
// lifted from `spike_t7_panzoom.rs` (WP-gpui-spike-T7's own feasibility
// spike), with one deliberate implementation change explained below.
//
// ── Why re-layout zoom, not GPU-transform zoom ───────────────────────────
// The spike's own zoom used raw `window.paint_quad`/shaped-text calls inside
// a `gpui::canvas()` paint closure, scaling screen-space coordinates by hand
// — the right call for its 500-node stress test (hundreds of individually
// laid-out `Div`s would exercise gpui's flex/taffy solver instead of the
// paint fast path). This crate's pinned gpui rev has no `Styled`-trait
// `.scale()`/transform method for an ordinary `Div` (grep-verified against
// the vendored `gpui` checkout: `scale()` exists only on `scene.rs`/
// `geometry.rs`/`path_builder.rs` primitives, none of them reachable from
// element-building code). `/canvas` and `/world` each render a handful of
// elements (one report card; a few dozen agent tokens at most), not hundreds
// — cheap enough to re-layout every render pass with sizes/positions
// computed as `base * zoom` plain `px()` values via ordinary styled `Div`s.
// This "discrete re-layout zoom" re-shapes text at every zoom level instead
// of blurring a rasterized scale — arguably better legibility than a true
// GPU transform would give, and it stays inside the styled-element API this
// crate already uses everywhere else (no `gpui::canvas()` paint closure
// needed for either page).
//
// Each page embeds one `PanZoomState` inside its own `gpui::Global` state
// struct (not a shared `Global` itself — `/canvas` and `/world` never share
// one camera) and wires the mouse/scroll handlers itself via `pan_zoom_area`
// below, mutating its own `cx.global_mut::<PageState>().panzoom` field —
// same "each page owns its own ~40-line wiring block, calling a small shared
// helper for the parts that are byte-identical" shape this crate already
// uses (`goals::relative_time`, `catalog_common::page_header`), rather than
// forcing a generic trait over two different `Global` types for one shared
// listener body.

use gpui::{point, px, Pixels, Point};

/// One page's camera state: where the virtual workspace has been panned to,
/// how far it is zoomed, and the in-flight drag bookkeeping `pan_zoom_area`
/// needs. `zoom` is a plain re-layout multiplier (see this module's header
/// comment) — callers scale their own `px()` values by it, never anything in
/// this struct itself.
#[derive(Debug, Clone, Copy)]
pub struct PanZoomState {
    pub pan: Point<Pixels>,
    pub zoom: f32,
    pub dragging: bool,
    pub drag_origin: Point<Pixels>,
    pub pan_at_drag_start: Point<Pixels>,
}

impl Default for PanZoomState {
    fn default() -> Self {
        // `Pixels`' inner field is `pub(crate)` to gpui — `px(0.0)` is the
        // public constructor every other call site in this crate already
        // uses, not a tuple-struct literal.
        Self {
            pan: point(px(0.0), px(0.0)),
            zoom: 1.0,
            dragging: false,
            drag_origin: point(px(0.0), px(0.0)),
            pan_at_drag_start: point(px(0.0), px(0.0)),
        }
    }
}

impl PanZoomState {
    /// "回正視角" — both pages' floating reset-view control.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Wheel-delta → zoom-factor math, shared so `/canvas` and `/world` scroll at
/// the same felt speed and clamp to the same [0.5, 2.0] range the spike
/// established. Kept as a free function (not a method taking `&mut
/// PanZoomState`) so each page's own `cx.listener` closure — which already
/// has to borrow its own `Global` type — applies the result itself.
pub fn wheel_zoom_factor(delta_y: f32) -> f32 {
    1.0 + delta_y * 0.0015
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_is_centered_unzoomed() {
        let s = PanZoomState::default();
        assert_eq!(s.zoom, 1.0);
        assert_eq!(s.pan, point(px(0.0), px(0.0)));
        assert!(!s.dragging);
    }

    #[test]
    fn reset_clears_pan_and_zoom_after_a_drag() {
        let mut s = PanZoomState::default();
        s.pan = point(px(120.0), px(-40.0));
        s.zoom = 1.8;
        s.dragging = true;
        s.reset();
        assert_eq!(s.zoom, 1.0);
        assert_eq!(s.pan, point(px(0.0), px(0.0)));
        assert!(!s.dragging);
    }

    #[test]
    fn wheel_zoom_factor_is_neutral_at_zero_delta() {
        assert_eq!(wheel_zoom_factor(0.0), 1.0);
    }

    #[test]
    fn wheel_zoom_factor_grows_with_positive_delta() {
        assert!(wheel_zoom_factor(100.0) > 1.0);
        assert!(wheel_zoom_factor(-100.0) < 1.0);
    }
}
