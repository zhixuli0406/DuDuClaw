//! Where the candidate window goes.
//!
//! text-input-v3 clients report a **cursor rectangle** — the caret's box, in
//! the client's own surface coordinates — and the input method draws its
//! candidate list relative to it. The compositor owns the final placement,
//! because only it knows the output the pair actually sits on.
//!
//! Everything here is pure geometry so it can be tested without a compositor,
//! a renderer, or a live fcitx5 — the same split `crate::decor` and
//! `crate::window_policy` use.

use smithay::utils::{Logical, Point, Rectangle, Size};

/// Gap between the caret and the candidate window, in logical pixels. Small
/// enough to read as attached to the text, large enough not to touch it.
pub const CARET_GAP: i32 = 2;

/// Top-left corner for a candidate window, in **space** coordinates.
///
/// * `caret` — the client-reported cursor rectangle, relative to `parent`.
/// * `parent` — the text-input surface's geometry on the space.
/// * `popup` — the candidate window's current size.
/// * `output` — the output being drawn, on the space.
///
/// Rules, in order:
/// 1. Sit just **below** the caret — where every desktop IME puts it, and the
///    one position that never covers the text being composed.
/// 2. If that would run off the bottom of the output, **flip above** the
///    caret. Flipping (rather than clamping) is what keeps the candidate list
///    off the very characters the user is looking at while typing at the
///    bottom of a screen.
/// 3. Clamp into the output on both axes, so a popup can never be pushed
///    entirely off screen by a client reporting a wild caret.
///
/// A popup larger than the output clamps to the output's top-left rather than
/// to a negative offset: showing the first candidates beats showing the last.
pub fn place(
    caret: Rectangle<i32, Logical>,
    parent: Rectangle<i32, Logical>,
    popup: Size<i32, Logical>,
    output: Rectangle<i32, Logical>,
) -> Point<i32, Logical> {
    let caret_top = parent.loc.y + caret.loc.y;
    let caret_bottom = caret_top + caret.size.h;

    let below = caret_bottom + CARET_GAP;
    let above = caret_top - CARET_GAP - popup.h;

    // Rule 2: only flip if there is genuinely no room below. If neither side
    // fits, stay below and let the clamp below deal with it — a downward
    // overhang is the more predictable of two bad options.
    let y = if below + popup.h > output.loc.y + output.size.h && above >= output.loc.y {
        above
    } else {
        below
    };

    let x = parent.loc.x + caret.loc.x;

    Point::from((
        clamp_axis(x, popup.w, output.loc.x, output.size.w),
        clamp_axis(y, popup.h, output.loc.y, output.size.h),
    ))
}

/// Clamps `start` so a `len`-long span stays inside `[origin, origin + extent)`,
/// preferring the low edge when the span simply does not fit.
fn clamp_axis(start: i32, len: i32, origin: i32, extent: i32) -> i32 {
    let max = origin + (extent - len).max(0);
    start.clamp(origin, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((x, y)), Size::from((w, h)))
    }

    /// A 1080p output at the space origin — the default single-screen case.
    fn output_1080p() -> Rectangle<i32, Logical> {
        rect(0, 0, 1920, 1080)
    }

    #[test]
    fn the_default_position_is_just_below_the_caret() {
        // Window at (100, 200); caret 30px down its own surface, 18px tall.
        let at = place(rect(40, 30, 2, 18), rect(100, 200, 800, 600), Size::from((240, 90)), output_1080p());
        assert_eq!(at.x, 140, "x follows the caret");
        assert_eq!(at.y, 200 + 30 + 18 + CARET_GAP);
    }

    #[test]
    fn a_caret_near_the_bottom_flips_the_popup_above_it() {
        // Caret bottom at 1070 — a 90px popup below would end at 1162.
        let at = place(rect(10, 1050, 2, 20), rect(0, 0, 1920, 1080), Size::from((240, 90)), output_1080p());
        assert_eq!(at.y, 1050 - CARET_GAP - 90);
        assert!(at.y >= 0);
    }

    #[test]
    fn a_popup_that_fits_nowhere_stays_below_and_is_clamped_on_screen() {
        // Output only 100px tall: neither below nor above fits.
        let output = rect(0, 0, 400, 100);
        let at = place(rect(0, 40, 2, 20), rect(0, 0, 400, 100), Size::from((200, 90)), output);
        assert!(at.y >= 0 && at.y + 90 <= 100, "clamped into the output: {at:?}");
        assert_eq!(at.y, 10);
    }

    #[test]
    fn a_caret_near_the_right_edge_pulls_the_popup_back_on_screen() {
        let at = place(rect(1900, 10, 2, 18), rect(0, 0, 1920, 1080), Size::from((240, 90)), output_1080p());
        assert_eq!(at.x, 1920 - 240);
    }

    #[test]
    fn a_negative_caret_offset_cannot_push_the_popup_off_the_left_edge() {
        let at = place(rect(-500, 10, 2, 18), rect(0, 0, 1920, 1080), Size::from((240, 90)), output_1080p());
        assert_eq!(at.x, 0);
    }

    #[test]
    fn a_popup_wider_than_the_output_pins_to_the_left_edge() {
        let output = rect(0, 0, 200, 1080);
        let at = place(rect(50, 10, 2, 18), rect(0, 0, 200, 1080), Size::from((400, 90)), output);
        assert_eq!(at.x, 0, "showing the first candidates beats showing the last");
    }

    #[test]
    fn an_output_that_does_not_start_at_the_origin_is_respected() {
        // Second monitor to the right of the first.
        let output = rect(1920, 0, 1280, 1024);
        let at = place(rect(0, 0, 2, 18), rect(1920, 0, 1280, 1024), Size::from((240, 90)), output);
        assert_eq!(at.x, 1920);
        let far_right = place(
            rect(1270, 0, 2, 18),
            rect(1920, 0, 1280, 1024),
            Size::from((240, 90)),
            output,
        );
        assert_eq!(far_right.x, 1920 + 1280 - 240);
    }

    #[test]
    fn a_zero_size_popup_is_placed_not_crashed() {
        let at = place(rect(10, 10, 2, 18), rect(0, 0, 800, 600), Size::from((0, 0)), output_1080p());
        assert_eq!(at, Point::from((10, 10 + 18 + CARET_GAP)));
    }

    #[test]
    fn a_zero_height_caret_still_lands_below_its_reported_top() {
        // Some clients report a bare insertion point with no height.
        let at = place(rect(10, 400, 0, 0), rect(0, 0, 800, 600), Size::from((240, 90)), output_1080p());
        assert_eq!(at.y, 400 + CARET_GAP);
    }

    #[test]
    fn clamp_axis_prefers_the_low_edge_when_the_span_does_not_fit() {
        assert_eq!(clamp_axis(50, 300, 0, 200), 0);
        assert_eq!(clamp_axis(-5, 100, 0, 200), 0);
        assert_eq!(clamp_axis(180, 100, 0, 200), 100);
        assert_eq!(clamp_axis(20, 100, 0, 200), 20);
    }
}
