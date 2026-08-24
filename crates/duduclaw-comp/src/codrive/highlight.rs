// CD-1 codrive — target highlight box.
// DESIGN-codrive-desktop-2026-08.md §3.3.2(b): "目標高亮框（點擊前 200ms
// 預示）" — task brief req 5 generalizes the fixed 200ms into a caller-set,
// clamped duration. A hollow amber border drawn around the rectangle the
// agent is about to act on, so a human watching the desktop sees where the
// next click/type is headed before it happens. Same render-element
// mechanism as `cursor.rs` (zero-texture `SolidColorRenderElement`s fed
// into `winit_backend.rs`'s custom_elements slice) — four thin bars
// forming a hollow rectangle rather than a filled one, since a filled
// highlight would obscure the very content it's meant to point at.

use std::time::Instant;

use smithay::{
    backend::renderer::element::{solid::SolidColorBuffer, solid::SolidColorRenderElement, Kind},
    utils::{Logical, Physical, Point, Rectangle, Scale},
};

use crate::state::DuduclawComp;

use super::cursor::AGENT_COLOR_LIVE;

/// Border thickness in logical pixels — thin enough not to obscure the
/// highlighted content, thick enough to read at a glance.
const BORDER_PX: f64 = 3.0;

/// `ms` default/clamp for the `highlight` op (task brief req 5: "ms 選填，
/// 預設 800、clamp [100, 5000]"). A free function (not inlined into the
/// `InjectCmd::Highlight` match arm in `mod.rs`) specifically so it's
/// directly unit-testable without spinning up seat/event-loop state.
pub fn clamp_highlight_ms(ms: Option<u64>) -> u64 {
    ms.unwrap_or(800).clamp(100, 5000)
}

impl DuduclawComp {
    /// Called once per redraw (`winit_backend.rs`). Returns this frame's
    /// highlight border elements — empty if there's no active highlight or
    /// its deadline has passed. As a side effect, clears
    /// `self.codrive_highlight` once expired ("過期就清掉", task brief req
    /// 5) so a stale highlight can't reappear on some later check before
    /// the next `highlight` op arrives.
    pub fn codrive_highlight_elements(&mut self, now: Instant, scale: Scale<f64>) -> Vec<SolidColorRenderElement> {
        self.codrive_highlight_elements_at(now, Point::from((0.0, 0.0)), scale)
    }

    /// A4-1: same as [`Self::codrive_highlight_elements`] but shifted by
    /// `offset` first.
    ///
    /// `codrive_highlight` is stored in the GLOBAL logical space of `Space`,
    /// while `render_output`'s `custom_elements` are interpreted in the
    /// rendered output's OWN space. Those coincide for an output at the
    /// origin — the winit backend's single output, and the single-monitor
    /// hardware case — so the zero-offset wrapper above is byte-identical to
    /// the pre-A4-1 behaviour. The udev backend passes `-output.loc` so a
    /// highlight box lands in the right place on a second monitor too.
    ///
    /// `scale` is the rendered output's own live scale
    /// (`render::output_render_scale`) — WP-comp-shell-display D4b-3
    /// replaced a hardcoded `Scale::from(1.0)`. Like the agent cursor cross,
    /// this element is `SolidColorRenderElement`-based (geometry baked in at
    /// construction, ignored at render time — see `codrive::cursor::
    /// build_agent_cursor_elements`'s doc for the smithay-source-checked
    /// reason), so both the border's PHYSICAL position and thickness come
    /// from whatever `scale` this caller supplies.
    pub fn codrive_highlight_elements_at(
        &mut self,
        now: Instant,
        offset: Point<f64, Logical>,
        scale: Scale<f64>,
    ) -> Vec<SolidColorRenderElement> {
        let Some((rect, deadline)) = self.codrive_highlight else {
            return Vec::new();
        };
        if now >= deadline {
            self.codrive_highlight = None;
            return Vec::new();
        }
        build_border(Rectangle::new(rect.loc + offset, rect.size), scale)
    }
}

fn build_border(rect: Rectangle<f64, Logical>, scale: Scale<f64>) -> Vec<SolidColorRenderElement> {
    let loc = rect.loc;
    let size = rect.size;
    let b = BORDER_PX;

    // Four thin bars forming a hollow rectangle: top, bottom, left, right.
    // Each tuple is (x, y, width, height) in logical space. For a
    // degenerately thin rect (w or h smaller than the border itself) the
    // top/bottom or left/right bars would overlap or invert — filtered out
    // below rather than drawing a negative-size element.
    let bars: [(f64, f64, f64, f64); 4] = [
        (loc.x, loc.y, size.w, b),
        (loc.x, loc.y + size.h - b, size.w, b),
        (loc.x, loc.y, b, size.h),
        (loc.x + size.w - b, loc.y, b, size.h),
    ];

    bars.iter()
        .filter(|(_, _, w, h)| *w > 0.0 && *h > 0.0)
        .map(|&(x, y, w, h)| {
            let px_w = (w.round() as i32).max(1);
            let px_h = (h.round() as i32).max(1);
            let buf = SolidColorBuffer::new((px_w, px_h), AGENT_COLOR_LIVE);
            let phys_loc: Point<i32, Physical> =
                Point::<f64, Logical>::from((x, y)).to_physical_precise_round(scale);
            SolidColorRenderElement::from_buffer(&buf, phys_loc, scale, 1.0, Kind::Unspecified)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight_ms_default_is_800() {
        assert_eq!(clamp_highlight_ms(None), 800);
    }

    #[test]
    fn highlight_ms_clamped_low() {
        assert_eq!(clamp_highlight_ms(Some(10)), 100);
    }

    #[test]
    fn highlight_ms_clamped_high() {
        assert_eq!(clamp_highlight_ms(Some(999_999)), 5000);
    }

    #[test]
    fn highlight_ms_passthrough_in_range() {
        assert_eq!(clamp_highlight_ms(Some(1200)), 1200);
    }

    #[test]
    fn highlight_ms_boundaries_are_inclusive() {
        assert_eq!(clamp_highlight_ms(Some(100)), 100);
        assert_eq!(clamp_highlight_ms(Some(5000)), 5000);
    }

    #[test]
    fn build_border_produces_four_bars_for_a_normal_rect() {
        let rect = Rectangle::<f64, Logical>::new(Point::from((10.0, 20.0)), smithay::utils::Size::from((100.0, 50.0)));
        let elems = build_border(rect, Scale::from(1.0));
        assert_eq!(elems.len(), 4, "a normal-sized rect should produce top/bottom/left/right bars");
    }

    /// WP-comp-shell-display D4b-3: `SolidColorRenderElement` bakes its
    /// geometry in at construction (ignores the render-time scale — see
    /// `codrive::cursor::build_agent_cursor_elements`'s doc), so a
    /// different `scale` argument here must produce a different physical
    /// geometry, or the "single source of truth" claim is untested.
    #[test]
    fn a_real_output_scale_changes_the_baked_geometry() {
        use smithay::backend::renderer::element::Element;
        let rect = Rectangle::<f64, Logical>::new(Point::from((10.0, 20.0)), smithay::utils::Size::from((100.0, 50.0)));
        let at_1x = build_border(rect, Scale::from(1.0));
        let at_2x = build_border(rect, Scale::from(2.0));
        assert_eq!(at_1x.len(), at_2x.len());
        for (a, b) in at_1x.iter().zip(at_2x.iter()) {
            assert_ne!(a.geometry(Scale::from(1.0)), b.geometry(Scale::from(1.0)));
        }
    }
}
