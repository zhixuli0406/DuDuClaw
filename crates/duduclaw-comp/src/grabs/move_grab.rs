// Adapted from smithay's `smallvil` example
// (`smallvil/src/grabs/move_grab.rs`), MIT License. See `main.rs` for the
// full attribution note.

use crate::{decor::DecorInsets, DuduclawComp};
use smithay::{
    desktop::Window,
    input::pointer::{
        AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
        GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
        GestureSwipeUpdateEvent, GrabStartData as PointerGrabStartData, MotionEvent, PointerGrab,
        PointerInnerHandle, RelativeMotionEvent,
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, Rectangle, Size},
};

/// WM-2: the bounds a title-bar drag is kept inside.
///
/// `None` on a grab means "unclamped", which is what a client-initiated
/// `xdg_toplevel.move` still gets: a client asking to be moved has its own
/// reasons (tear-off tabs, drag-to-attach), and second-guessing it is not this
/// work package's business. The compositor's OWN title-bar drag is clamped,
/// because that one is the human moving a window and there is no way to
/// recover a window that has been thrown off the screen — this compositor has
/// no task switcher that can move anything back.
#[derive(Debug, Clone, Copy)]
pub struct MoveClamp {
    /// The work area the frame must stay reachable inside.
    pub work: Rectangle<i32, Logical>,
    /// The frame size at grab time. A drag never resizes, so capturing it once
    /// is exact and avoids re-reading `Space` on every motion event.
    pub frame_size: Size<i32, Logical>,
    /// This window's decoration insets, i.e. how to convert between the frame
    /// the human sees and the content rectangle `Space` maps.
    pub insets: DecorInsets,
}

/// Pure half of the clamped drag: given a desired **content** origin, produce
/// the content origin actually allowed.
///
/// Split out (rather than inlined into `motion`) for the usual reason in this
/// crate — a `PointerGrab` cannot be exercised in a unit test without a real
/// `Seat`, but the arithmetic that decides where a dragged window may land is
/// exactly the part worth pinning down. See `codrive/window_target.rs`'s
/// module doc for the original statement of that split.
pub fn clamped_move_target(
    desired_content: Point<f64, Logical>,
    clamp: Option<MoveClamp>,
) -> Point<i32, Logical> {
    let desired = desired_content.to_i32_round();
    let Some(c) = clamp else {
        return desired;
    };
    let frame_loc = Point::from((desired.x - c.insets.left, desired.y - c.insets.top));
    let clamped = crate::decor::clamp_frame_loc(c.frame_size, c.work, frame_loc, c.insets);
    Point::from((clamped.x + c.insets.left, clamped.y + c.insets.top))
}

pub struct MoveSurfaceGrab {
    pub start_data: PointerGrabStartData<DuduclawComp>,
    pub window: Window,
    pub initial_window_location: Point<i32, Logical>,
    /// WM-2: see [`MoveClamp`]. `None` reproduces the pre-WM-2 behaviour byte
    /// for byte.
    pub clamp: Option<MoveClamp>,
}

impl PointerGrab<DuduclawComp> for MoveSurfaceGrab {
    fn motion(
        &mut self,
        data: &mut DuduclawComp,
        handle: &mut PointerInnerHandle<'_, DuduclawComp>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        // While the grab is active, no client has pointer focus
        handle.motion(data, None, event);

        let delta = event.location - self.start_data.location;
        let new_location = self.initial_window_location.to_f64() + delta;
        let new_location = clamped_move_target(new_location, self.clamp);
        data.space
            .map_element(self.window.clone(), new_location, true);
        // WM-2: keep the remembered floating frame in step with where the
        // window actually is, so a later output change refits it where the
        // human left it. No-op for an unplaced or maximized window.
        data.decor_sync_frame(&self.window);
    }

    fn relative_motion(
        &mut self,
        data: &mut DuduclawComp,
        handle: &mut PointerInnerHandle<'_, DuduclawComp>,
        focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, focus, event);
    }

    fn button(
        &mut self,
        data: &mut DuduclawComp,
        handle: &mut PointerInnerHandle<'_, DuduclawComp>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);

        // The button is a button code as defined in the
        // Linux kernel's linux/input-event-codes.h header file, e.g. BTN_LEFT.
        const BTN_LEFT: u32 = 0x110;

        if !handle.current_pressed().contains(&BTN_LEFT) {
            // No more buttons are pressed, release the grab.
            handle.unset_grab(self, data, event.serial, event.time, true);
        }
    }

    fn axis(
        &mut self,
        data: &mut DuduclawComp,
        handle: &mut PointerInnerHandle<'_, DuduclawComp>,
        details: AxisFrame,
    ) {
        handle.axis(data, details)
    }

    fn frame(&mut self, data: &mut DuduclawComp, handle: &mut PointerInnerHandle<'_, DuduclawComp>) {
        handle.frame(data);
    }

    fn gesture_swipe_begin(
        &mut self,
        data: &mut DuduclawComp,
        handle: &mut PointerInnerHandle<'_, DuduclawComp>,
        event: &GestureSwipeBeginEvent,
    ) {
        handle.gesture_swipe_begin(data, event)
    }

    fn gesture_swipe_update(
        &mut self,
        data: &mut DuduclawComp,
        handle: &mut PointerInnerHandle<'_, DuduclawComp>,
        event: &GestureSwipeUpdateEvent,
    ) {
        handle.gesture_swipe_update(data, event)
    }

    fn gesture_swipe_end(
        &mut self,
        data: &mut DuduclawComp,
        handle: &mut PointerInnerHandle<'_, DuduclawComp>,
        event: &GestureSwipeEndEvent,
    ) {
        handle.gesture_swipe_end(data, event)
    }

    fn gesture_pinch_begin(
        &mut self,
        data: &mut DuduclawComp,
        handle: &mut PointerInnerHandle<'_, DuduclawComp>,
        event: &GesturePinchBeginEvent,
    ) {
        handle.gesture_pinch_begin(data, event)
    }

    fn gesture_pinch_update(
        &mut self,
        data: &mut DuduclawComp,
        handle: &mut PointerInnerHandle<'_, DuduclawComp>,
        event: &GesturePinchUpdateEvent,
    ) {
        handle.gesture_pinch_update(data, event)
    }

    fn gesture_pinch_end(
        &mut self,
        data: &mut DuduclawComp,
        handle: &mut PointerInnerHandle<'_, DuduclawComp>,
        event: &GesturePinchEndEvent,
    ) {
        handle.gesture_pinch_end(data, event)
    }

    fn gesture_hold_begin(
        &mut self,
        data: &mut DuduclawComp,
        handle: &mut PointerInnerHandle<'_, DuduclawComp>,
        event: &GestureHoldBeginEvent,
    ) {
        handle.gesture_hold_begin(data, event)
    }

    fn gesture_hold_end(
        &mut self,
        data: &mut DuduclawComp,
        handle: &mut PointerInnerHandle<'_, DuduclawComp>,
        event: &GestureHoldEndEvent,
    ) {
        handle.gesture_hold_end(data, event)
    }

    fn start_data(&self) -> &PointerGrabStartData<DuduclawComp> {
        &self.start_data
    }

    fn unset(&mut self, _data: &mut DuduclawComp) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((x, y)), Size::from((w, h)))
    }

    fn ssd_clamp() -> MoveClamp {
        MoveClamp {
            // The appliance's own work area: 1280x800 minus the shell's bands.
            work: rect(0, 30, 1280, 680),
            frame_size: Size::from((1024, 578)),
            insets: DecorInsets::SSD,
        }
    }

    #[test]
    fn an_unclamped_grab_rounds_and_otherwise_does_nothing() {
        // This is the pre-WM-2 behaviour, still used for client-initiated
        // `xdg_toplevel.move`.
        let out = clamped_move_target(Point::from((-4000.5, 9000.4)), None);
        assert_eq!((out.x, out.y), (-4001, 9000));
    }

    #[test]
    fn a_clamped_grab_keeps_the_title_bar_below_the_menu_bar() {
        // Dragging hard upwards: the frame top is pinned to the work area top,
        // so the CONTENT origin lands one full decoration below it.
        let out = clamped_move_target(Point::from((200.0, -5000.0)), Some(ssd_clamp()));
        assert_eq!(out.y, 30 + DecorInsets::SSD.top);
    }

    #[test]
    fn a_clamped_grab_keeps_the_title_bar_above_the_dock() {
        let out = clamped_move_target(Point::from((200.0, 5000.0)), Some(ssd_clamp()));
        // Frame top pinned at work.bottom - insets.top; content is one
        // decoration below that, i.e. exactly the work area's bottom edge.
        assert_eq!(out.y, 710);
    }

    #[test]
    fn a_clamped_grab_leaves_a_grabbable_sliver_at_both_screen_edges() {
        let c = ssd_clamp();
        let left = clamped_move_target(Point::from((-9000.0, 100.0)), Some(c));
        assert_eq!(left.x, -c.frame_size.w + crate::decor::MIN_ON_SCREEN_PX + c.insets.left);
        let right = clamped_move_target(Point::from((9000.0, 100.0)), Some(c));
        assert_eq!(right.x, 1280 - crate::decor::MIN_ON_SCREEN_PX + c.insets.left);
    }

    #[test]
    fn a_drag_inside_the_work_area_is_untouched_apart_from_rounding() {
        let out = clamped_move_target(Point::from((300.4, 200.6)), Some(ssd_clamp()));
        assert_eq!((out.x, out.y), (300, 201));
    }

    #[test]
    fn an_undecorated_clamped_window_is_still_kept_reachable() {
        let clamp = MoveClamp {
            work: rect(0, 30, 1280, 680),
            frame_size: Size::from((640, 480)),
            insets: DecorInsets::NONE,
        };
        let out = clamped_move_target(Point::from((0.0, 99_999.0)), Some(clamp));
        assert_eq!(out.y, 710 - crate::decor::MIN_ON_SCREEN_PX);
    }
}
