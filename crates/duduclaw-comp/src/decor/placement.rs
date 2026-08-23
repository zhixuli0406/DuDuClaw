//! WM-2: where a new floating window goes.
//!
//! WM-1 filled every non-shell toplevel into the whole work area. That made
//! "two windows" indistinguishable from "one window" and made the notion of a
//! title bar pointless. This module is the replacement rule the user picked
//! (option **B**, 浮動視窗):
//!
//! * size = 80 % of the work area on each axis, floored;
//! * position = centred, then **cascaded** by 24 px per already-placed window;
//! * the cascade **wraps** rather than walking off the edge — a rule that
//!   exists because "超出工作區則繞回" is in the task brief, and because a
//!   window placed outside the work area would be born unreachable.
//!
//! Every function here is pure and takes plain rectangles, per this crate's
//! standing "pure logic unit-tested, live `Space`/`Seat` state live-run
//! verified" split (see `codrive/window_target.rs`'s module doc for the
//! original statement of that rule).

use smithay::utils::{Logical, Point, Rectangle, Size};

use super::{DecorInsets, MIN_CONTENT_H, MIN_CONTENT_W};

/// Percentage of the work area a floating window occupies on each axis.
pub const FLOAT_PERCENT: i32 = 80;

/// Cascade step, in logical pixels, per already-placed window.
pub const CASCADE_STEP: i32 = 24;

/// The frame size a new floating window is born with.
///
/// 80 % on each axis, floored (integer division), then floored again at the
/// minimum usable content size plus this window's decoration, and finally
/// capped at the work area itself — that last clamp is what keeps a
/// pathologically small output (see `window_policy::MIN_APP_HEIGHT`, which
/// already refuses to band such an output at all) from producing a frame
/// larger than the screen.
pub fn floating_frame_size(
    work: Rectangle<i32, Logical>,
    insets: DecorInsets,
) -> Size<i32, Logical> {
    let min_w = MIN_CONTENT_W + insets.horizontal();
    let min_h = MIN_CONTENT_H + insets.vertical();
    let w = (work.size.w * FLOAT_PERCENT / 100)
        .max(min_w)
        .min(work.size.w)
        .max(1);
    let h = (work.size.h * FLOAT_PERCENT / 100)
        .max(min_h)
        .min(work.size.h)
        .max(1);
    Size::from((w, h))
}

/// How many cascade steps fit between the centred base position and the
/// bottom-right corner of the work area.
///
/// Returns `0` when the frame is so large (or the work area so small) that
/// even one step would push it out — in that case every window lands on the
/// centred position, which is correct: stacking is better than off-screen.
pub fn cascade_slots(work: Rectangle<i32, Logical>, frame: Size<i32, Logical>) -> i32 {
    let base = centred_loc(work, frame);
    let room_x = (work.loc.x + work.size.w) - (base.x + frame.w);
    let room_y = (work.loc.y + work.size.h) - (base.y + frame.h);
    (room_x / CASCADE_STEP).min(room_y / CASCADE_STEP).max(0)
}

/// The centred origin for a frame of this size inside this work area.
pub fn centred_loc(work: Rectangle<i32, Logical>, frame: Size<i32, Logical>) -> Point<i32, Logical> {
    Point::from((
        work.loc.x + (work.size.w - frame.w) / 2,
        work.loc.y + (work.size.h - frame.h) / 2,
    ))
}

/// The frame rectangle for the `index`-th floating window placed on this work
/// area.
///
/// `index` is a monotonically increasing counter (`DecorState::cascade_next`);
/// this function does the wrapping, so the caller never has to reset it — and
/// deliberately so, because a counter that is reset when the last window
/// closes would make two consecutive windows land on top of each other in the
/// common "close one, open another" flow.
pub fn cascade_frame_rect(
    work: Rectangle<i32, Logical>,
    insets: DecorInsets,
    index: u32,
) -> Rectangle<i32, Logical> {
    let size = floating_frame_size(work, insets);
    let base = centred_loc(work, size);
    let slots = cascade_slots(work, size);
    // `slots + 1` positions exist: the base itself plus `slots` steps.
    let n = (index % (slots as u32 + 1)) as i32;
    Rectangle::new(
        Point::from((base.x + CASCADE_STEP * n, base.y + CASCADE_STEP * n)),
        size,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((x, y)), Size::from((w, h)))
    }

    /// The real appliance geometry: 1280x800 output minus the shell's 30px
    /// menu bar and 90px dock (`window_policy::ReservedBands::default`).
    fn appliance_work() -> Rectangle<i32, Logical> {
        rect(0, 30, 1280, 680)
    }

    #[test]
    fn a_floating_window_is_eighty_percent_of_the_work_area() {
        let size = floating_frame_size(appliance_work(), DecorInsets::SSD);
        assert_eq!((size.w, size.h), (1024, 544));
    }

    #[test]
    fn the_eighty_percent_is_floored_not_rounded() {
        // 999 * 80 / 100 = 799.2 -> 799, never 800.
        let size = floating_frame_size(rect(0, 0, 999, 999), DecorInsets::SSD);
        assert_eq!((size.w, size.h), (799, 799));
    }

    #[test]
    fn a_tiny_work_area_never_yields_a_frame_larger_than_itself() {
        // 80% of 200 is 160, below the 240px minimum content width, so the
        // minimum kicks in — and is then capped back to the work area.
        let work = rect(0, 0, 200, 150);
        let size = floating_frame_size(work, DecorInsets::SSD);
        assert!(size.w <= work.size.w && size.h <= work.size.h, "got {size:?}");
        assert!(size.w >= 1 && size.h >= 1);
    }

    #[test]
    fn the_minimum_content_size_is_honoured_when_the_work_area_allows_it() {
        // 80% of 400 = 320 wide, but 80% of 180 = 144 tall which is under the
        // 160px content minimum + 34px of decoration.
        let size = floating_frame_size(rect(0, 0, 400, 400), DecorInsets::SSD);
        assert_eq!(size.w, 320);
        assert_eq!(size.h, 320);
        let short = floating_frame_size(rect(0, 0, 400, 200), DecorInsets::SSD);
        assert_eq!(short.h, MIN_CONTENT_H + DecorInsets::SSD.vertical());
    }

    #[test]
    fn the_first_window_is_centred() {
        let work = appliance_work();
        let r = cascade_frame_rect(work, DecorInsets::SSD, 0);
        assert_eq!((r.size.w, r.size.h), (1024, 544));
        assert_eq!(r.loc.x, (1280 - 1024) / 2);
        assert_eq!(r.loc.y, 30 + (680 - 544) / 2);
        // Centred means equal margins on both sides.
        assert_eq!(r.loc.x - work.loc.x, (work.loc.x + work.size.w) - (r.loc.x + r.size.w));
    }

    #[test]
    fn each_subsequent_window_steps_down_and_right_by_twenty_four() {
        let work = appliance_work();
        let a = cascade_frame_rect(work, DecorInsets::SSD, 0);
        let b = cascade_frame_rect(work, DecorInsets::SSD, 1);
        assert_eq!(b.loc.x - a.loc.x, CASCADE_STEP);
        assert_eq!(b.loc.y - a.loc.y, CASCADE_STEP);
        assert_eq!(b.size, a.size, "cascading must not change the size");
    }

    #[test]
    fn the_cascade_wraps_instead_of_walking_off_the_work_area() {
        let work = appliance_work();
        let slots = cascade_slots(work, floating_frame_size(work, DecorInsets::SSD));
        assert!(slots >= 1, "the appliance geometry must have room for a cascade");
        let first = cascade_frame_rect(work, DecorInsets::SSD, 0);
        let wrapped = cascade_frame_rect(work, DecorInsets::SSD, slots as u32 + 1);
        assert_eq!(wrapped, first, "index {} must wrap back to the base", slots + 1);
    }

    #[test]
    fn every_cascade_position_stays_fully_inside_the_work_area() {
        let work = appliance_work();
        for index in 0..64u32 {
            let r = cascade_frame_rect(work, DecorInsets::SSD, index);
            assert!(r.loc.x >= work.loc.x, "index {index}: {r:?} off the left");
            assert!(r.loc.y >= work.loc.y, "index {index}: {r:?} off the top");
            assert!(
                r.loc.x + r.size.w <= work.loc.x + work.size.w,
                "index {index}: {r:?} off the right"
            );
            assert!(
                r.loc.y + r.size.h <= work.loc.y + work.size.h,
                "index {index}: {r:?} off the bottom"
            );
        }
    }

    #[test]
    fn a_work_area_with_no_room_to_cascade_stacks_every_window_centred() {
        // A frame that exactly fills the work area leaves zero room.
        let work = rect(0, 0, 300, 200);
        let size = floating_frame_size(work, DecorInsets::SSD);
        assert_eq!(cascade_slots(work, size), 0);
        let a = cascade_frame_rect(work, DecorInsets::SSD, 0);
        let b = cascade_frame_rect(work, DecorInsets::SSD, 7);
        assert_eq!(a, b, "no room to step means everything lands centred");
    }

    #[test]
    fn a_second_output_keeps_its_own_origin() {
        // udev maps additional connectors side by side at (w, 0) — the same
        // case `window_policy`'s own tests cover for the work area.
        let work = rect(1280, 30, 1920, 960);
        let r = cascade_frame_rect(work, DecorInsets::SSD, 0);
        assert!(r.loc.x >= 1280, "placed at {r:?}, which is on the first output");
    }

    #[test]
    fn an_undecorated_window_gets_the_same_frame_size_as_a_decorated_one() {
        // The 80% rule is about the FRAME, so a client-side-decorated window
        // occupies exactly as much screen as a server-decorated one.
        let work = appliance_work();
        assert_eq!(
            floating_frame_size(work, DecorInsets::NONE),
            floating_frame_size(work, DecorInsets::SSD)
        );
    }
}
