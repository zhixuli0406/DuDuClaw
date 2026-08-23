//! WM-3: **resize by dragging a window's edge**.
//!
//! WM-2's known-limitations list said it plainly: *"No resize by dragging the
//! border. A 1 px border is not a resize target."* This module is the answer —
//! and the shape of the answer is the interesting part.
//!
//! ## The hot zone is the drop shadow, not the client area
//!
//! The obvious implementation is "the outer 8 px **inside** the frame". It is
//! also wrong: on a server-decorated window those 8 px are the client's own
//! surface everywhere except the title bar, so a scrollbar, a resize corner
//! drawn by the toolkit, or a list item flush against the window edge would
//! stop receiving clicks. That is a regression a user notices immediately and
//! cannot work around.
//!
//! So the hot zone sits **outside** the frame instead, filling exactly the
//! [`SHADOW_PX`](super::SHADOW_PX)-wide drop-shadow ring WM-2 already draws
//! (they are both 8 px, and deliberately so — the affordance is visible). This
//! is the standard "invisible/extended resize border" every mainstream
//! compositor uses, and it steals nothing from anybody: [`hit_frame_edge`]
//! returns `None` for any point inside the frame.
//!
//! ## Two deliberate consequences
//!
//! * **A window's resize ring overlaps whatever is beneath it.** `frame_hit_at`
//!   walks the stack top-down, so a point in window A's ring that also lies
//!   over window B's content resizes A. That is the same trade every extended
//!   border makes; the alternative (ring loses to any surface below) would make
//!   the ring unusable the moment windows overlap, which floating placement
//!   makes the normal case.
//! * **The ring is clipped to the work area** ([`hit_frame_edge_in_work`]).
//!   Without it, a window sitting near the top of the work area would put an
//!   8 px resize strip over the shell's menu bar, and clicking the menu bar
//!   would resize a window. It also means a **maximized** window — whose frame
//!   *is* the work area, so whose ring lies entirely outside it — cannot be
//!   edge-resized at all, which is the correct behaviour and costs no extra
//!   code.

use smithay::utils::{Logical, Point, Rectangle};

use super::{DecorInsets, SHADOW_PX};

/// Thickness of the resize ring outside the frame, logical pixels. Equal to
/// the drop shadow's extent on purpose — the ring is exactly the shadow, so the
/// affordance is where the eye already sees a boundary.
pub const RESIZE_HOT_PX: i32 = SHADOW_PX;

/// How far along a perpendicular edge a point still counts as a corner.
pub const RESIZE_CORNER_PX: i32 = 24;

/// Which edge (or corner) of a window's frame a pointer is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameEdge {
    Top,
    Bottom,
    Left,
    Right,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl FrameEdge {
    /// Does dragging this edge move the frame's **top** edge? (These are the
    /// edges whose drag moves the window's origin, which is what
    /// `grabs::resize_grab::handle_commit` compensates for.)
    pub fn moves_top(self) -> bool {
        matches!(self, Self::Top | Self::TopLeft | Self::TopRight)
    }

    /// Does dragging this edge move the frame's **left** edge?
    pub fn moves_left(self) -> bool {
        matches!(self, Self::Left | Self::TopLeft | Self::BottomLeft)
    }

    /// Log-safe name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Bottom => "bottom",
            Self::Left => "left",
            Self::Right => "right",
            Self::TopLeft => "top-left",
            Self::TopRight => "top-right",
            Self::BottomLeft => "bottom-left",
            Self::BottomRight => "bottom-right",
        }
    }
}

/// Assembles the four booleans into an edge. `None` when nothing was hit (all
/// false) — an impossible combination for a point genuinely in the ring, but
/// this function does not get to assume its caller is correct.
fn combine(top: bool, bottom: bool, left: bool, right: bool) -> Option<FrameEdge> {
    match (top, bottom, left, right) {
        (true, _, true, _) => Some(FrameEdge::TopLeft),
        (true, _, _, true) => Some(FrameEdge::TopRight),
        (_, true, true, _) => Some(FrameEdge::BottomLeft),
        (_, true, _, true) => Some(FrameEdge::BottomRight),
        (true, _, _, _) => Some(FrameEdge::Top),
        (_, true, _, _) => Some(FrameEdge::Bottom),
        (_, _, true, _) => Some(FrameEdge::Left),
        (_, _, _, true) => Some(FrameEdge::Right),
        _ => None,
    }
}

/// Classifies a pointer position against one window's resize ring.
///
/// `None` for an undecorated window (a client-side-decorated one owns its own
/// resize edges via `xdg_toplevel.resize`, and second-guessing it is not this
/// compositor's business), for a point inside the frame, and for a point
/// further than [`RESIZE_HOT_PX`] outside it.
pub fn hit_frame_edge(
    frame: Rectangle<i32, Logical>,
    insets: DecorInsets,
    pos: Point<f64, Logical>,
) -> Option<FrameEdge> {
    if !insets.is_decorated() {
        return None;
    }
    if frame.size.w <= 0 || frame.size.h <= 0 {
        return None;
    }

    let outer = Rectangle::new(
        Point::from((frame.loc.x - RESIZE_HOT_PX, frame.loc.y - RESIZE_HOT_PX)),
        smithay::utils::Size::from((
            frame.size.w + 2 * RESIZE_HOT_PX,
            frame.size.h + 2 * RESIZE_HOT_PX,
        )),
    );
    if !outer.to_f64().contains(pos) {
        return None;
    }
    if frame.to_f64().contains(pos) {
        // The client's own pixels (or the title bar). Not ours to grab.
        return None;
    }

    let (fx0, fy0) = (frame.loc.x as f64, frame.loc.y as f64);
    let (fx1, fy1) = (
        (frame.loc.x + frame.size.w) as f64,
        (frame.loc.y + frame.size.h) as f64,
    );

    let mut top = pos.y < fy0;
    let mut bottom = pos.y >= fy1;
    let mut left = pos.x < fx0;
    let mut right = pos.x >= fx1;

    let corner = RESIZE_CORNER_PX as f64;
    // A point on one side's band is promoted to a corner when it is also within
    // `RESIZE_CORNER_PX` of a perpendicular frame edge. Corners are the hardest
    // targets to hit, so they get the generous zone.
    if left || right {
        if pos.y < fy0 + corner {
            top = true;
        } else if pos.y >= fy1 - corner {
            bottom = true;
        }
    }
    if top || bottom {
        if pos.x < fx0 + corner {
            left = true;
        } else if pos.x >= fx1 - corner {
            right = true;
        }
    }

    combine(top, bottom, left, right)
}

/// [`hit_frame_edge`] clipped to the work area.
///
/// `work = None` means "no real output yet", and then nothing is clipped — the
/// same fail-open choice `input::begin_titlebar_move` makes for its clamp
/// (better an unclipped ring than one clipped against a rectangle we invented).
pub fn hit_frame_edge_in_work(
    frame: Rectangle<i32, Logical>,
    insets: DecorInsets,
    work: Option<Rectangle<i32, Logical>>,
    pos: Point<f64, Logical>,
) -> Option<FrameEdge> {
    if let Some(work) = work {
        if !work.to_f64().contains(pos) {
            return None;
        }
    }
    hit_frame_edge(frame, insets, pos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::utils::Size;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((x, y)), Size::from((w, h)))
    }

    fn p(x: f64, y: f64) -> Point<f64, Logical> {
        Point::from((x, y))
    }

    /// A frame well inside a generous work area, so the work-area clip never
    /// interferes with the pure edge tests.
    fn frame() -> Rectangle<i32, Logical> {
        rect(200, 200, 400, 300)
    }

    #[test]
    fn the_ring_is_exactly_the_drop_shadows_width() {
        // If these ever diverge the affordance stops matching what is drawn.
        assert_eq!(RESIZE_HOT_PX, SHADOW_PX);
        assert_eq!(RESIZE_HOT_PX, 8);
    }

    #[test]
    fn a_point_inside_the_frame_is_never_a_resize_target() {
        // The whole reason the ring is outside: the client's pixels stay the
        // client's. Includes the title bar and the 1px border itself.
        for pos in [p(400.0, 350.0), p(200.0, 200.0), p(599.0, 499.0), p(400.0, 205.0)] {
            assert_eq!(
                hit_frame_edge(frame(), DecorInsets::SSD, pos),
                None,
                "{pos:?} is inside the frame and must fall through"
            );
        }
    }

    #[test]
    fn each_of_the_four_sides_is_hit_from_just_outside_the_frame() {
        let f = frame();
        assert_eq!(hit_frame_edge(f, DecorInsets::SSD, p(400.0, 196.0)), Some(FrameEdge::Top));
        assert_eq!(hit_frame_edge(f, DecorInsets::SSD, p(400.0, 503.0)), Some(FrameEdge::Bottom));
        assert_eq!(hit_frame_edge(f, DecorInsets::SSD, p(196.0, 350.0)), Some(FrameEdge::Left));
        assert_eq!(hit_frame_edge(f, DecorInsets::SSD, p(603.0, 350.0)), Some(FrameEdge::Right));
    }

    #[test]
    fn the_corners_are_bidirectional_and_generous() {
        let f = frame();
        assert_eq!(hit_frame_edge(f, DecorInsets::SSD, p(196.0, 196.0)), Some(FrameEdge::TopLeft));
        assert_eq!(hit_frame_edge(f, DecorInsets::SSD, p(603.0, 196.0)), Some(FrameEdge::TopRight));
        assert_eq!(hit_frame_edge(f, DecorInsets::SSD, p(196.0, 503.0)), Some(FrameEdge::BottomLeft));
        assert_eq!(hit_frame_edge(f, DecorInsets::SSD, p(603.0, 503.0)), Some(FrameEdge::BottomRight));
        // Still a corner 20px along the left band (inside RESIZE_CORNER_PX).
        assert_eq!(hit_frame_edge(f, DecorInsets::SSD, p(196.0, 219.0)), Some(FrameEdge::TopLeft));
        // And a plain edge just past it.
        assert_eq!(hit_frame_edge(f, DecorInsets::SSD, p(196.0, 240.0)), Some(FrameEdge::Left));
    }

    #[test]
    fn a_point_further_out_than_the_ring_is_not_a_hit() {
        let f = frame();
        assert_eq!(hit_frame_edge(f, DecorInsets::SSD, p(400.0, 191.0)), None);
        assert_eq!(hit_frame_edge(f, DecorInsets::SSD, p(608.5, 350.0)), None);
    }

    #[test]
    fn an_undecorated_window_has_no_server_side_resize_ring() {
        // A CSD client keeps its own resize edges through xdg_toplevel.resize;
        // adding a second, invisible one around it would fight the toolkit.
        assert_eq!(hit_frame_edge(frame(), DecorInsets::NONE, p(196.0, 350.0)), None);
    }

    #[test]
    fn a_degenerate_frame_produces_no_hits_instead_of_a_panic() {
        assert_eq!(hit_frame_edge(rect(0, 0, 0, 0), DecorInsets::SSD, p(-2.0, -2.0)), None);
    }

    #[test]
    fn the_boundary_between_frame_and_ring_is_one_pixel_wide_in_its_decision() {
        let f = frame();
        // 199.5 is outside the frame (frame starts at 200) -> ring.
        assert!(hit_frame_edge(f, DecorInsets::SSD, p(400.0, 199.5)).is_some());
        // 200.0 is the frame's first row -> not ours.
        assert_eq!(hit_frame_edge(f, DecorInsets::SSD, p(400.0, 200.0)), None);
    }

    #[test]
    fn the_ring_is_clipped_to_the_work_area() {
        // A window whose top edge sits 4px below the work area: the ring above
        // it would otherwise cover the shell's menu bar.
        let work = rect(0, 30, 1280, 680);
        let f = rect(200, 34, 400, 300);
        // 4px above the frame top -> inside the work area, still a resize.
        assert_eq!(
            hit_frame_edge_in_work(f, DecorInsets::SSD, Some(work), p(400.0, 31.0)),
            Some(FrameEdge::Top)
        );
        // 2px above the work area -> the menu bar's pixels, not the window's.
        assert_eq!(
            hit_frame_edge_in_work(f, DecorInsets::SSD, Some(work), p(400.0, 28.0)),
            None
        );
    }

    #[test]
    fn a_maximized_window_cannot_be_edge_resized() {
        // Its frame IS the work area, so the entire ring lies outside it.
        let work = rect(0, 30, 1280, 680);
        let maximized = work;
        for pos in [p(640.0, 27.0), p(640.0, 713.0), p(-3.0, 300.0), p(1283.0, 300.0)] {
            assert_eq!(
                hit_frame_edge_in_work(maximized, DecorInsets::SSD, Some(work), pos),
                None,
                "{pos:?} must not resize a maximized window"
            );
        }
    }

    #[test]
    fn with_no_output_yet_the_ring_is_not_clipped_at_all() {
        let f = frame();
        assert_eq!(
            hit_frame_edge_in_work(f, DecorInsets::SSD, None, p(400.0, 196.0)),
            Some(FrameEdge::Top)
        );
    }

    #[test]
    fn the_origin_moving_edges_are_exactly_top_and_left() {
        for e in [FrameEdge::Top, FrameEdge::TopLeft, FrameEdge::TopRight] {
            assert!(e.moves_top(), "{}", e.as_str());
        }
        for e in [FrameEdge::Bottom, FrameEdge::BottomLeft, FrameEdge::BottomRight] {
            assert!(!e.moves_top(), "{}", e.as_str());
        }
        for e in [FrameEdge::Left, FrameEdge::TopLeft, FrameEdge::BottomLeft] {
            assert!(e.moves_left(), "{}", e.as_str());
        }
        for e in [FrameEdge::Right, FrameEdge::TopRight, FrameEdge::BottomRight] {
            assert!(!e.moves_left(), "{}", e.as_str());
        }
    }
}
