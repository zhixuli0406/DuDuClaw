//! A2 共駕復活 (2026-08-24) — the screen-edge "AI 駕駛中" indicator.
//!
//! DESIGN-codrive-desktop-2026-08.md §3.3.2(d): "系統級『共駕中』指示…不可隱藏".
//! A2 contract §5, second paragraph: four 3 px border bars framing the whole
//! output — amber while the agent is driving ([`DrivingMode::CoDrive`]), dark
//! red while the human holds the wheel ([`DrivingMode::Handover`]), and
//! **nothing at all** in [`DrivingMode::Human`], where there is no co-drive
//! session to indicate.
//!
//! ## Why this is compositor-drawn, and what "不可隱藏" actually buys
//! Same mechanism as `cursor.rs`/`highlight.rs`: zero-texture
//! `SolidColorRenderElement`s handed to the backend's `custom_elements`
//! slice. No client surface, no protocol, nothing an application can bind to,
//! move, restyle, or occlude — `decor::paint::build_output_elements` keeps
//! every custom element AHEAD of every window and every layer surface in the
//! render list, and this crate treats earlier elements as nearer the viewer.
//! A fullscreen client therefore cannot paint over the frame, which is the
//! entire point: the human's evidence that an agent has the wheel must not be
//! something the agent (or anything it launched) can turn off.
//!
//! ## Coordinate space — no output offset here, deliberately
//! Unlike `codrive_highlight_elements_at`, this takes NO offset. A highlight
//! box is stored in the GLOBAL logical space of `Space` and must be
//! translated into the output being rendered; this frame is defined
//! **relative to the output itself** (its own top-left corner and its own
//! mode size), which is already the space `render_output`'s custom elements
//! are interpreted in. Adding `-output.loc` here would push the frame of a
//! second monitor off its own screen. The udev backend therefore passes each
//! surface's own output size and no offset; the winit backend passes its
//! window size, and its single output sits at the origin anyway.

use smithay::{
    backend::renderer::element::{solid::SolidColorBuffer, solid::SolidColorRenderElement, Kind},
    utils::{Logical, Physical, Point, Scale, Size},
};

use super::cursor::{AGENT_COLOR_FROZEN, AGENT_COLOR_LIVE};
use super::mode::DrivingMode;

/// Border thickness in logical pixels (A2 contract §5: "四條 3px 邊框條").
/// Thin enough to cost the user almost no screen area, thick enough to read
/// from across a room — this is meant to be noticed peripherally, not read.
const INDICATOR_PX: i32 = 3;

/// The four bars, as `(x, y, w, h)` in the output's OWN logical space.
///
/// Top and bottom span the full width; left and right span the full height,
/// so the four overlap at the corners rather than mitring — an overlap of
/// identical colour is invisible, and mitring would add arithmetic with no
/// visual difference. Pure and integer-only so the geometry is unit-testable
/// with no renderer, matching `highlight::build_border`'s own split.
///
/// A degenerate output (either dimension ≤ 0) yields nothing: there is no
/// frame to draw, and emitting a negative-size element would be a rendering
/// bug rather than an honest empty answer. Bars that would be non-positive on
/// a pathologically small output are dropped for the same reason.
fn indicator_bars(width: i32, height: i32, thickness: i32) -> Vec<(i32, i32, i32, i32)> {
    if width <= 0 || height <= 0 || thickness <= 0 {
        return Vec::new();
    }
    let b = thickness.min(width).min(height);
    [
        (0, 0, width, b),
        (0, height - b, width, b),
        (0, 0, b, height),
        (width - b, 0, b, height),
    ]
    .into_iter()
    .filter(|&(_, _, w, h)| w > 0 && h > 0)
    .collect()
}

/// This frame's screen-edge indicator elements for `mode`.
///
/// `output_size` MUST be the true LOGICAL size of the output being
/// rendered — i.e. physical mode size divided by the output's own current
/// scale, exactly what `Space::output_geometry(output).size` already
/// computes. WP-comp-shell-display D4b-3: before this round every caller
/// fed this the raw PHYSICAL mode/window size mislabeled as `Logical`,
/// which only happened to be correct because scale was always 1.0 — with a
/// real scale in play that size is TWICE (or however-many-times) too big,
/// and `scale` below would multiply it up again on top of that.
///
/// `scale` is the output's own live scale (`render::output_render_scale`).
/// This element is `SolidColorRenderElement`-based (geometry baked in at
/// construction — see `codrive::cursor::build_agent_cursor_elements`'s doc
/// for the smithay-source-checked reason), so both the frame's PHYSICAL
/// position and thickness come from it.
pub fn build_mode_indicator_elements(
    output_size: Size<i32, Logical>,
    mode: DrivingMode,
    scale: Scale<f64>,
) -> Vec<SolidColorRenderElement> {
    let color = match mode {
        // No session, nothing to indicate. Drawing a frame here would tell
        // the human an agent has the wheel when none does.
        DrivingMode::Human => return Vec::new(),
        DrivingMode::CoDrive => AGENT_COLOR_LIVE,
        DrivingMode::Handover => AGENT_COLOR_FROZEN,
    };

    indicator_bars(output_size.w, output_size.h, INDICATOR_PX)
        .into_iter()
        .map(|(x, y, w, h)| {
            let buf = SolidColorBuffer::new((w, h), color);
            let loc: Point<i32, Physical> =
                Point::<f64, Logical>::from((x as f64, y as f64)).to_physical_precise_round(scale);
            SolidColorRenderElement::from_buffer(&buf, loc, scale, 1.0, Kind::Unspecified)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_mode_draws_nothing_at_all() {
        assert!(
            build_mode_indicator_elements(Size::from((1920, 1080)), DrivingMode::Human, Scale::from(1.0)).is_empty()
        );
    }

    #[test]
    fn codrive_and_handover_both_draw_four_bars() {
        for mode in [DrivingMode::CoDrive, DrivingMode::Handover] {
            let elems = build_mode_indicator_elements(Size::from((1920, 1080)), mode, Scale::from(1.0));
            assert_eq!(elems.len(), 4, "{mode:?} should frame the whole output");
        }
    }

    /// WP-comp-shell-display D4b-3: `SolidColorRenderElement` bakes its
    /// geometry in at construction, so a different `scale` argument must
    /// produce a different physical geometry — see `codrive::cursor::
    /// build_agent_cursor_elements`'s doc for the smithay-source-checked
    /// reason this is asserted directly rather than assumed.
    #[test]
    fn a_real_output_scale_changes_the_baked_geometry() {
        use smithay::backend::renderer::element::Element;
        let at_1x = build_mode_indicator_elements(Size::from((1920, 1080)), DrivingMode::CoDrive, Scale::from(1.0));
        let at_2x = build_mode_indicator_elements(Size::from((1920, 1080)), DrivingMode::CoDrive, Scale::from(2.0));
        assert_eq!(at_1x.len(), at_2x.len());
        for (a, b) in at_1x.iter().zip(at_2x.iter()) {
            assert_ne!(a.geometry(Scale::from(1.0)), b.geometry(Scale::from(1.0)));
        }
    }

    #[test]
    fn the_two_active_modes_use_the_two_established_agent_colours() {
        // One shared pair of constants with `cursor.rs`, so the frame and the
        // ghost cursor can never disagree about which state the desktop is in.
        assert_ne!(AGENT_COLOR_LIVE, AGENT_COLOR_FROZEN);
    }

    #[test]
    fn bars_frame_the_output_at_its_own_origin_not_a_global_offset() {
        let bars = indicator_bars(1920, 1080, 3);
        assert_eq!(bars.len(), 4);
        assert_eq!(bars[0], (0, 0, 1920, 3), "top");
        assert_eq!(bars[1], (0, 1077, 1920, 3), "bottom");
        assert_eq!(bars[2], (0, 0, 3, 1080), "left");
        assert_eq!(bars[3], (1917, 0, 3, 1080), "right");
    }

    #[test]
    fn a_degenerate_output_yields_no_bars_rather_than_negative_geometry() {
        assert!(indicator_bars(0, 1080, 3).is_empty());
        assert!(indicator_bars(1920, 0, 3).is_empty());
        assert!(indicator_bars(-1, -1, 3).is_empty());
        assert!(indicator_bars(1920, 1080, 0).is_empty());
    }

    #[test]
    fn a_tiny_output_clamps_the_thickness_instead_of_inverting_a_bar() {
        // 2×2 with a 3 px border: every bar would otherwise be placed at a
        // negative offset or given a negative size.
        let bars = indicator_bars(2, 2, 3);
        assert_eq!(bars.len(), 4);
        for (x, y, w, h) in bars {
            assert!(x >= 0 && y >= 0, "({x},{y}) must stay on screen");
            assert!(w > 0 && h > 0, "({w}×{h}) must be a real rectangle");
        }
    }

    #[test]
    fn every_bar_stays_inside_the_output() {
        for (w, h) in [(1920, 1080), (800, 600), (4, 4), (3, 100), (100, 3)] {
            for (x, y, bw, bh) in indicator_bars(w, h, INDICATOR_PX) {
                assert!(x + bw <= w, "bar overflows width on {w}×{h}");
                assert!(y + bh <= h, "bar overflows height on {w}×{h}");
            }
        }
    }
}
