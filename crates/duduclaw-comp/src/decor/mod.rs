//! WM-2 (2026-08-23): **server-side decorations and floating windows**.
//!
//! ## What changed, and why
//!
//! WM-1 was explicitly transitional: an ordinary application window was
//! *filled* into the reserved-band work area (see `crate::window_policy`) and
//! comp answered `zxdg_decoration_manager_v1` with a flat `ClientSide` because
//! it had no decoration renderer at all. That produced a desktop where every
//! window was the same size, in the same place, stacked on top of each other,
//! and where a client that draws no decorations of its own (there are several)
//! had no title, no close button and no way to be moved.
//!
//! The user's decision for this round was **"B：浮動視窗＋完整窗感"**. So:
//!
//! * new non-shell toplevels are **floating** — 80 % of the work area,
//!   centred, cascaded (`placement`);
//! * comp now answers `ServerSide` by default and **draws** the decoration:
//!   a 32 px title bar with the live `xdg_toplevel.title`, a close button, a
//!   1 px border and a soft drop shadow (`paint`, `text`);
//! * the title bar is draggable, and the drag is clamped so a window can never
//!   be thrown somewhere it cannot be grabbed back from.
//!
//! The session shell (`window_policy::SHELL_APP_ID`) is untouched by all of
//! this: it stays full-output, undecorated, and exempt from every rule here.
//!
//! ## The geometry model (one sentence, then the picture)
//!
//! **`Space` still maps the CONTENT rectangle**, exactly as before WM-2; the
//! decoration is drawn *around* that rectangle and exists nowhere in
//! `Space`'s own bookkeeping.
//!
//! ```text
//!  frame.loc ──►┌───────────────────────────────────────┐ ▲ shadow (8px, outside)
//!               │ 1px border                            │ │
//!               │ ┌───────────────────────────────────┐ │ │
//!               │ │ title bar, 32px      [ ✕ ]        │ │ │
//!               │ ├───────────────────────────────────┤ │ │
//!               │ │                                   │ │ │
//!  content.loc ─┼─┼──►  the client's own surface      │ │ │
//!               │ │     (this is what Space maps)     │ │ │
//!               │ └───────────────────────────────────┘ │ │
//!               └───────────────────────────────────────┘ ▼
//! ```
//!
//! That choice is deliberate and it is the reason this work package does not
//! touch `codrive/`, `shell_control/`, `input.rs`'s surface routing, or the
//! move/resize grabs' coordinate math: every existing caller of
//! `Space::element_location` / `element_geometry` / `element_under` /
//! `surface_under` keeps meaning exactly what it meant before. The
//! alternative — wrapping `Window` in a decorated `SpaceElement` so that
//! `Space` maps the *frame* — is the more "idiomatic smithay" shape, and it
//! was rejected for this round precisely because it would have rewritten the
//! coordinate assumptions of every one of those already-live-verified call
//! sites at once.
//!
//! The one thing that model costs us is that `Space::element_under` cannot
//! see the title bar (it is not a surface and not inside any window's bbox).
//! That is handled explicitly and testably by [`hit_frame`] plus
//! `DuduclawComp::frame_hit_at` (`input.rs`), which run *before* the ordinary
//! surface routing — see those two for the ordering rule.
//!
//! ## Palette
//!
//! Calm Glass / brand light surfaces from the root `CLAUDE.md` "Aesthetic
//! Direction" table: `stone-100` title bar, `stone-900` title text,
//! `stone-300` border, amber only where the brand already uses it. There is
//! deliberately **no dark mode**: comp has no theme mechanism at all today
//! (the cursor's `source`/`size` preferences are the only visual settings it
//! carries, and they are per-property, not a theme). Wiring one is a
//! standalone piece of work — see the `TODO(theme)` note on [`Palette`].

pub mod mode;
pub mod paint;
pub mod placement;
pub mod text;
pub mod xmark;

use smithay::utils::{Logical, Point, Rectangle, Size};

pub use mode::{negotiated_ssd, DecorMode};
pub use placement::cascade_frame_rect;

/// Height of the server-side title bar, in logical pixels, **excluding** the
/// 1 px border above it. Task brief: "SSD 標題列（32px…）".
pub const TITLE_BAR_H: i32 = 32;

/// Window border thickness, in logical pixels. Task brief: "1px stone-300
/// 邊框".
pub const BORDER_PX: i32 = 1;

/// One shadow ring's thickness, in logical pixels.
pub const SHADOW_RING_PX: i32 = 2;

/// Per-ring alpha of the drop shadow, innermost first. Four 2 px rings = the
/// "8px 漸層" the task brief asked for, approximated with a stepped ramp
/// rather than a real gradient: a gradient would need either a per-window
/// texture upload (allocation proportional to window size, every resize) or a
/// shader this crate does not have. Four solid rings are four
/// `SolidColorRenderElement`s per edge — the same primitive
/// `codrive/highlight.rs` already draws its target box with.
pub const SHADOW_ALPHAS: [f32; 4] = [0.10, 0.06, 0.035, 0.015];

/// Total shadow extent outside the border, in logical pixels.
pub const SHADOW_PX: i32 = SHADOW_RING_PX * SHADOW_ALPHAS.len() as i32;

/// Width of the close button's hit target inside the title bar.
///
/// GNOME's own close button sits in a ~38 px box; 46 px is a little more
/// generous because this compositor is also driven by an agent pointer and by
/// touch-grade absolute positioning in the VM, where a few pixels of slop are
/// the difference between "closes the window" and "starts dragging it".
pub const CLOSE_BTN_W: i32 = 46;

/// Left padding before the title text starts.
pub const TITLE_PAD_LEFT: i32 = 12;

/// Gap kept between the end of the title text and the close button.
pub const TITLE_GAP_RIGHT: i32 = 8;

/// Title text size in logical pixels.
pub const TITLE_FONT_PX: f32 = 13.0;

/// How much of a window's frame must stay inside the work area when it is
/// dragged. Below this a window is effectively lost: there is no task
/// switcher UI in this compositor (Super+Tab cycles focus but does not move
/// anything), so an off-screen window can only be recovered by closing it.
pub const MIN_ON_SCREEN_PX: i32 = 64;

/// Smallest content size a floating placement will ever configure.
pub const MIN_CONTENT_W: i32 = 240;
/// See [`MIN_CONTENT_W`].
pub const MIN_CONTENT_H: i32 = 160;

/// RGBA colours used by the decoration, as premultiplied-irrelevant opaque
/// `f32` quadruples (`SolidColorBuffer` takes `Color32F`).
///
/// TODO(theme): comp has no theme mechanism, so these are the light-surface
/// values only. When one lands (a `shell_control` op plus a persisted
/// preference, mirroring `cursor/source.rs` + `cursor/persist.rs`), this
/// struct is the single place to switch — nothing else in the crate hard-codes
/// a decoration colour.
pub struct Palette;

impl Palette {
    /// `stone-100` (`#f5f5f4`) — the focused title bar.
    pub const TITLE_BAR_ACTIVE: [f32; 4] = [0.961, 0.961, 0.957, 1.0];
    /// `stone-200` (`#e7e5e4`) — an unfocused window's title bar. Only the
    /// background changes with focus; the text colour stays put so the cached
    /// glyph raster (see [`paint`]) does not have to be re-rasterised every
    /// time focus moves.
    pub const TITLE_BAR_INACTIVE: [f32; 4] = [0.906, 0.898, 0.894, 1.0];
    /// `stone-900` (`#1c1917`) — title text and the resting close glyph.
    pub const TITLE_TEXT: [u8; 3] = [0x1c, 0x19, 0x17];
    /// `stone-300` (`#d6d3d1`) — the 1 px border.
    pub const BORDER: [f32; 4] = [0.839, 0.827, 0.820, 1.0];
    /// `rose-600` (`#e11d48`) — close button hover fill (the one place this
    /// palette leaves the warm neutrals, because "this destroys work" is the
    /// one affordance that must not read as neutral).
    pub const CLOSE_HOVER_BG: [f32; 4] = [0.882, 0.114, 0.282, 1.0];
    /// White — the close glyph while hovered.
    pub const CLOSE_HOVER_GLYPH: [u8; 3] = [0xff, 0xff, 0xff];
    /// Shadow colour (alpha comes from [`SHADOW_ALPHAS`]).
    pub const SHADOW_RGB: [f32; 3] = [0.0, 0.0, 0.0];
}

/// How much bigger the frame is than the content, on each side.
///
/// [`DecorInsets::NONE`] is a client-side-decorated (or shell, or shadow
/// workspace) window: frame == content, nothing is drawn, and every geometry
/// function in this module degrades to the identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecorInsets {
    pub top: i32,
    pub left: i32,
    pub right: i32,
    pub bottom: i32,
}

impl DecorInsets {
    /// No decoration at all.
    pub const NONE: Self = Self {
        top: 0,
        left: 0,
        right: 0,
        bottom: 0,
    };

    /// The server-side decoration: a border all round, and the title bar
    /// stacked above the content inside the top border.
    pub const SSD: Self = Self {
        top: TITLE_BAR_H + BORDER_PX,
        left: BORDER_PX,
        right: BORDER_PX,
        bottom: BORDER_PX,
    };

    #[inline]
    pub fn for_ssd(ssd: bool) -> Self {
        if ssd {
            Self::SSD
        } else {
            Self::NONE
        }
    }

    /// True when this window is actually decorated by the compositor.
    #[inline]
    pub fn is_decorated(&self) -> bool {
        self.top > 0
    }

    #[inline]
    pub fn horizontal(&self) -> i32 {
        self.left + self.right
    }

    #[inline]
    pub fn vertical(&self) -> i32 {
        self.top + self.bottom
    }
}

/// The frame rectangle for a window whose content occupies `content`.
pub fn frame_rect(content: Rectangle<i32, Logical>, insets: DecorInsets) -> Rectangle<i32, Logical> {
    Rectangle::new(
        Point::from((content.loc.x - insets.left, content.loc.y - insets.top)),
        Size::from((
            content.size.w + insets.horizontal(),
            content.size.h + insets.vertical(),
        )),
    )
}

/// The content rectangle inside a given frame — the inverse of
/// [`frame_rect`].
///
/// The size is floored at 1×1: a frame too small to contain its own
/// decoration would otherwise produce a zero or negative `xdg_toplevel`
/// configure, which is a protocol error waiting to happen rather than a small
/// window.
pub fn content_rect(frame: Rectangle<i32, Logical>, insets: DecorInsets) -> Rectangle<i32, Logical> {
    Rectangle::new(
        Point::from((frame.loc.x + insets.left, frame.loc.y + insets.top)),
        Size::from((
            (frame.size.w - insets.horizontal()).max(1),
            (frame.size.h - insets.vertical()).max(1),
        )),
    )
}

/// The frame plus its drop shadow — what actually has to intersect an output
/// for this window to be worth rendering there.
pub fn shadow_bounds(frame: Rectangle<i32, Logical>, insets: DecorInsets) -> Rectangle<i32, Logical> {
    if !insets.is_decorated() {
        return frame;
    }
    Rectangle::new(
        Point::from((frame.loc.x - SHADOW_PX, frame.loc.y - SHADOW_PX)),
        Size::from((
            frame.size.w + 2 * SHADOW_PX,
            frame.size.h + 2 * SHADOW_PX,
        )),
    )
}

/// The title bar rectangle, in the same coordinate space as `frame`.
///
/// `None` for an undecorated window. The bar sits *inside* the border, which
/// is why it is inset by [`BORDER_PX`] on three sides — the border is drawn as
/// a ring around the whole frame, so a title bar drawn at the frame's own
/// edges would paint over it.
pub fn title_bar_rect(
    frame: Rectangle<i32, Logical>,
    insets: DecorInsets,
) -> Option<Rectangle<i32, Logical>> {
    if !insets.is_decorated() {
        return None;
    }
    let w = frame.size.w - insets.horizontal();
    if w <= 0 {
        return None;
    }
    Some(Rectangle::new(
        Point::from((frame.loc.x + insets.left, frame.loc.y + BORDER_PX)),
        Size::from((w, TITLE_BAR_H)),
    ))
}

/// The close button's rectangle inside a title bar.
///
/// Right-aligned and full-height. On a title bar narrower than the button the
/// button takes the whole bar rather than overflowing — a 60 px-wide window is
/// pathological, but "the close button hangs outside the frame" would be a
/// visible bug and "there is no close button at all" would be a trap.
pub fn close_button_rect(title_bar: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
    let w = CLOSE_BTN_W.min(title_bar.size.w).max(1);
    Rectangle::new(
        Point::from((title_bar.loc.x + title_bar.size.w - w, title_bar.loc.y)),
        Size::from((w, title_bar.size.h)),
    )
}

/// The rectangle the title text may occupy: from the left padding up to the
/// close button, minus a gap. Width can legitimately come back `0` on a very
/// narrow window, in which case no text is rasterised at all.
pub fn title_text_rect(title_bar: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
    let close = close_button_rect(title_bar);
    let x = title_bar.loc.x + TITLE_PAD_LEFT;
    let right = close.loc.x - TITLE_GAP_RIGHT;
    Rectangle::new(
        Point::from((x, title_bar.loc.y)),
        Size::from(((right - x).max(0), title_bar.size.h)),
    )
}

/// What a pointer press on a window's frame means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameHit {
    /// The close button — send `xdg_toplevel.close`.
    Close,
    /// Anywhere else on the title bar — start a move grab.
    TitleBar,
}

/// Classifies a pointer position against one window's frame.
///
/// Returns `None` for a position that is not on the compositor's own
/// decoration — including a position inside the *content* area, which must
/// fall through to the ordinary surface routing untouched.
pub fn hit_frame(
    frame: Rectangle<i32, Logical>,
    insets: DecorInsets,
    pos: Point<f64, Logical>,
) -> Option<FrameHit> {
    let bar = title_bar_rect(frame, insets)?;
    if !bar.to_f64().contains(pos) {
        return None;
    }
    if close_button_rect(bar).to_f64().contains(pos) {
        Some(FrameHit::Close)
    } else {
        Some(FrameHit::TitleBar)
    }
}

/// Clamps a proposed frame origin so the window stays recoverable.
///
/// Three rules, all of them about "can the human get this window back":
///
/// 1. The frame's top edge never rises above the work area's top edge, so the
///    title bar is never pushed under the shell's menu bar.
/// 2. The frame's top edge never sinks past `work.bottom - grab_height`, so at
///    least the full title bar is always on screen.
/// 3. Horizontally, at least [`MIN_ON_SCREEN_PX`] of the frame stays inside
///    the work area on whichever side it is being dragged towards.
///
/// For an undecorated window (`insets == NONE`) rule 2 uses
/// [`MIN_ON_SCREEN_PX`] as the notional grab height — there is no title bar to
/// keep visible, but a client-decorated window still has its own one at the
/// top of its surface.
pub fn clamp_frame_loc(
    frame_size: Size<i32, Logical>,
    work: Rectangle<i32, Logical>,
    desired: Point<i32, Logical>,
    insets: DecorInsets,
) -> Point<i32, Logical> {
    let grab_h = if insets.is_decorated() {
        insets.top
    } else {
        MIN_ON_SCREEN_PX
    };

    let y_min = work.loc.y;
    let y_max = (work.loc.y + work.size.h - grab_h).max(y_min);

    let x_min = work.loc.x - frame_size.w + MIN_ON_SCREEN_PX;
    let x_max = work.loc.x + work.size.w - MIN_ON_SCREEN_PX;
    // A frame wider than the work area plus both margins can invert the range.
    let x_max = x_max.max(x_min);

    Point::from((desired.x.clamp(x_min, x_max), desired.y.clamp(y_min, y_max)))
}

/// Shrinks a frame so it fits inside `work`, keeping its origin where the
/// clamp allows. Used when the output's mode changes under already-placed
/// windows (`window_policy::reapply_window_policy_all`).
pub fn refit_frame(
    frame: Rectangle<i32, Logical>,
    work: Rectangle<i32, Logical>,
    insets: DecorInsets,
) -> Rectangle<i32, Logical> {
    let size = Size::from((
        frame.size.w.min(work.size.w).max(insets.horizontal() + 1),
        frame.size.h.min(work.size.h).max(insets.vertical() + 1),
    ));
    Rectangle::new(clamp_frame_loc(size, work, frame.loc, insets), size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((x, y)), Size::from((w, h)))
    }

    fn p(x: f64, y: f64) -> Point<f64, Logical> {
        Point::from((x, y))
    }

    #[test]
    fn ssd_insets_are_the_title_bar_plus_the_border() {
        // If either constant moves, `paint.rs`'s bar/border placement moves
        // with it — asserting the arithmetic here is what keeps the two in
        // step.
        assert_eq!(DecorInsets::SSD.top, 33);
        assert_eq!(DecorInsets::SSD.left, 1);
        assert_eq!(DecorInsets::SSD.right, 1);
        assert_eq!(DecorInsets::SSD.bottom, 1);
        assert_eq!(DecorInsets::SSD.horizontal(), 2);
        assert_eq!(DecorInsets::SSD.vertical(), 34);
    }

    #[test]
    fn frame_and_content_are_exact_inverses() {
        let content = rect(100, 200, 800, 600);
        for insets in [DecorInsets::SSD, DecorInsets::NONE] {
            let frame = frame_rect(content, insets);
            assert_eq!(content_rect(frame, insets), content, "insets {insets:?}");
        }
    }

    #[test]
    fn an_undecorated_window_has_frame_equal_to_content() {
        let content = rect(10, 20, 300, 400);
        assert_eq!(frame_rect(content, DecorInsets::NONE), content);
        assert_eq!(shadow_bounds(content, DecorInsets::NONE), content);
        assert_eq!(title_bar_rect(content, DecorInsets::NONE), None);
        assert_eq!(hit_frame(content, DecorInsets::NONE, p(15.0, 25.0)), None);
    }

    #[test]
    fn the_border_bottom_edge_sits_exactly_one_pixel_below_the_content() {
        // The whole frame model rests on this: content.bottom + 1 == the last
        // row of the frame, which is where `paint.rs` draws the bottom border.
        let content = rect(0, 0, 640, 480);
        let frame = frame_rect(content, DecorInsets::SSD);
        assert_eq!(frame.loc.y + frame.size.h - 1, content.loc.y + content.size.h);
        assert_eq!(frame.loc.x + frame.size.w - 1, content.loc.x + content.size.w);
    }

    #[test]
    fn the_title_bar_sits_inside_the_border_and_directly_above_the_content() {
        let content = rect(50, 100, 800, 600);
        let frame = frame_rect(content, DecorInsets::SSD);
        let bar = title_bar_rect(frame, DecorInsets::SSD).expect("decorated");
        assert_eq!(bar.loc.x, frame.loc.x + BORDER_PX);
        assert_eq!(bar.loc.y, frame.loc.y + BORDER_PX);
        assert_eq!(bar.size.w, frame.size.w - 2 * BORDER_PX);
        assert_eq!(bar.size.h, TITLE_BAR_H);
        // No gap and no overlap between the bar and the client's first row.
        assert_eq!(bar.loc.y + bar.size.h, content.loc.y);
    }

    #[test]
    fn the_close_button_is_right_aligned_inside_the_title_bar() {
        let bar = rect(0, 0, 800, TITLE_BAR_H);
        let close = close_button_rect(bar);
        assert_eq!(close.size.w, CLOSE_BTN_W);
        assert_eq!(close.loc.x + close.size.w, bar.loc.x + bar.size.w);
        assert_eq!(close.size.h, TITLE_BAR_H);
    }

    #[test]
    fn a_title_bar_narrower_than_the_button_still_has_a_close_button() {
        let bar = rect(0, 0, 20, TITLE_BAR_H);
        let close = close_button_rect(bar);
        assert_eq!(close.size.w, 20, "the button shrinks rather than overflowing");
        assert_eq!(close.loc.x, 0);
        // And the text area collapses instead of going negative.
        assert_eq!(title_text_rect(bar).size.w, 0);
    }

    #[test]
    fn hit_test_distinguishes_close_from_drag_from_content() {
        let content = rect(100, 100, 800, 600);
        let frame = frame_rect(content, DecorInsets::SSD);
        let bar = title_bar_rect(frame, DecorInsets::SSD).unwrap();

        // Left end of the bar: drag.
        assert_eq!(
            hit_frame(frame, DecorInsets::SSD, p(bar.loc.x as f64 + 5.0, bar.loc.y as f64 + 5.0)),
            Some(FrameHit::TitleBar)
        );
        // Right end of the bar: close.
        let close = close_button_rect(bar);
        assert_eq!(
            hit_frame(
                frame,
                DecorInsets::SSD,
                p(close.loc.x as f64 + 5.0, close.loc.y as f64 + 5.0)
            ),
            Some(FrameHit::Close)
        );
        // Inside the client area: not ours.
        assert_eq!(
            hit_frame(frame, DecorInsets::SSD, p(400.0, 400.0)),
            None,
            "a click in the content area must fall through to the surface"
        );
        // Outside the frame entirely.
        assert_eq!(hit_frame(frame, DecorInsets::SSD, p(0.0, 0.0)), None);
    }

    #[test]
    fn the_close_button_boundary_is_exactly_one_pixel_wide_in_its_decision() {
        let content = rect(0, 100, 800, 600);
        let frame = frame_rect(content, DecorInsets::SSD);
        let bar = title_bar_rect(frame, DecorInsets::SSD).unwrap();
        let close = close_button_rect(bar);
        let y = bar.loc.y as f64 + 1.0;
        assert_eq!(
            hit_frame(frame, DecorInsets::SSD, p(close.loc.x as f64 - 0.5, y)),
            Some(FrameHit::TitleBar)
        );
        assert_eq!(
            hit_frame(frame, DecorInsets::SSD, p(close.loc.x as f64, y)),
            Some(FrameHit::Close)
        );
    }

    #[test]
    fn a_drag_can_never_push_the_title_bar_above_the_work_area() {
        let work = rect(0, 30, 1280, 680);
        let size = Size::from((800, 634));
        let out = clamp_frame_loc(size, work, Point::from((100, -500)), DecorInsets::SSD);
        assert_eq!(out.y, 30, "the frame top is pinned to the work area top");
    }

    #[test]
    fn a_drag_always_leaves_the_whole_title_bar_on_screen_at_the_bottom() {
        let work = rect(0, 30, 1280, 680);
        let size = Size::from((800, 634));
        let out = clamp_frame_loc(size, work, Point::from((100, 99_999)), DecorInsets::SSD);
        // work bottom (710) minus the full title-bar-plus-border height (33).
        assert_eq!(out.y, 710 - DecorInsets::SSD.top);
    }

    #[test]
    fn a_drag_keeps_a_grabbable_sliver_on_both_horizontal_edges() {
        let work = rect(0, 30, 1280, 680);
        let size = Size::from((800, 634));
        let left = clamp_frame_loc(size, work, Point::from((-99_999, 100)), DecorInsets::SSD);
        assert_eq!(left.x, -800 + MIN_ON_SCREEN_PX);
        let right = clamp_frame_loc(size, work, Point::from((99_999, 100)), DecorInsets::SSD);
        assert_eq!(right.x, 1280 - MIN_ON_SCREEN_PX);
    }

    #[test]
    fn a_position_already_inside_the_work_area_is_left_alone() {
        let work = rect(0, 30, 1280, 680);
        let size = Size::from((800, 634));
        let want = Point::from((200, 60));
        assert_eq!(clamp_frame_loc(size, work, want, DecorInsets::SSD), want);
    }

    #[test]
    fn clamping_never_inverts_on_a_frame_larger_than_the_work_area() {
        // A 4000px-wide frame on a 640px work area: `x_min` would exceed
        // `x_max` without the guard, and `i32::clamp` panics on an inverted
        // range. This is reachable for real — an output whose mode shrinks
        // under an already-placed window.
        let work = rect(0, 0, 640, 200);
        let size = Size::from((4000, 3000));
        let out = clamp_frame_loc(size, work, Point::from((0, 0)), DecorInsets::SSD);
        assert!(out.x <= 0 && out.y >= 0, "clamped to {out:?} without panicking");
    }

    #[test]
    fn an_undecorated_window_is_still_clamped_by_a_notional_grab_height() {
        let work = rect(0, 0, 1280, 800);
        let size = Size::from((400, 300));
        let out = clamp_frame_loc(size, work, Point::from((0, 99_999)), DecorInsets::NONE);
        assert_eq!(out.y, 800 - MIN_ON_SCREEN_PX);
    }

    #[test]
    fn refit_shrinks_an_oversized_frame_into_the_work_area() {
        let work = rect(0, 30, 1280, 680);
        let big = rect(-200, -200, 4000, 4000);
        let out = refit_frame(big, work, DecorInsets::SSD);
        assert_eq!((out.size.w, out.size.h), (1280, 680));
        assert!(out.loc.y >= work.loc.y);
    }

    #[test]
    fn refit_leaves_a_frame_that_already_fits_untouched() {
        let work = rect(0, 30, 1280, 680);
        let ok = rect(100, 60, 800, 600);
        assert_eq!(refit_frame(ok, work, DecorInsets::SSD), ok);
    }

    #[test]
    fn the_shadow_extends_eight_pixels_beyond_the_frame_on_every_side() {
        let frame = rect(100, 100, 400, 300);
        let outer = shadow_bounds(frame, DecorInsets::SSD);
        assert_eq!(SHADOW_PX, 8, "four 2px rings");
        assert_eq!((outer.loc.x, outer.loc.y), (92, 92));
        assert_eq!((outer.size.w, outer.size.h), (416, 316));
    }
}
