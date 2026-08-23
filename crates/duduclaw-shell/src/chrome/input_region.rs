// Chrome-surface pointer input regions — D9-bug (2026-08-24).
//
// ── The bug this module exists for ───────────────────────────────────────
// The dock is a `zwlr_layer_surface_v1` anchored `bottom+left+right` at
// `DOCK_HEIGHT` (90 px), so on the appliance's 1280×800 output it is a
// 1280×90 band. It only PAINTS a centred pill roughly a third that wide.
// A Wayland surface's default input region is its ENTIRE surface, so every
// click landing in the transparent remainder of that band was swallowed by
// the dock and never reached the window underneath it. Measured on the
// final image in the VM: Chromium's first-run ToS "Accept" button sits at
// y≈760 on an 800 px output — inside the dock's 710–800 band — and two
// rounds of clicking it did nothing at all. Any application content that
// happens to fall in that band was equally unclickable.
//
// This is the same class of defect `windows::apply_bar_visibility` already
// records three generations of, one step further along: a bar that is
// HIDDEN must not accept input (that part was fixed), and a bar that is
// SHOWN must not accept input for the pixels it does not paint (this part).
//
// ── Why the dock's region is applied from prepaint ───────────────────────
// The pill's width is a function of how many apps the dock is currently
// showing (`home_dock::DOCK_MAX_APPS` is a cap, not a count — an appliance
// with three apps installed draws a narrower pill than one with eight) plus
// the agent avatars and the settings tile. Rather than re-derive that
// geometry from the layout constants — a second source of truth that would
// silently drift the moment a tile size or gap changes — the region is read
// back from the REAL laid-out pill via `Div::on_children_prepainted`, which
// hands over `window.layout_bounds(..)` for each child in window
// coordinates. That is the same coordinate space `Window::set_input_region`
// documents, so it needs no conversion.
//
// Applying it from inside prepaint (rather than caching it for the next
// render pass) is deliberate: a render pass triggered by an app-count
// change would otherwise apply the PREVIOUS pill's rect and then have no
// reason to render again, leaving the region permanently one layout behind.
//
// ── Why the applied region is cached ─────────────────────────────────────
// gpui's Wayland backend creates a fresh `wl_region`, fills it, hands it to
// `wl_surface::set_input_region` and commits on EVERY call. The dock's
// prepaint runs on every frame, so applying unconditionally would mean a
// region round trip plus a surface commit per frame, forever. `apply` below
// therefore no-ops whenever the wanted region is byte-identical to what is
// already applied.
//
// The cache is a `thread_local!` keyed by `RegionSlot`, not per-window
// state, because exactly one menu-bar surface and one dock surface exist
// per process by construction (`windows::SurfaceView::reconcile_chrome_bars`
// opens each ONCE and never closes it) and because the prepaint listener is
// only ever handed `&mut Window`/`&mut App` — it has no route back to the
// `SurfaceView` that owns the window. gpui drives all window rendering from
// one thread, so a thread-local is the right scope: two threads would mean
// two independent shells, each wanting its own cache anyway.

use std::cell::Cell;
use std::thread::LocalKey;

use gpui::{point, px, size, Bounds, Pixels, Window};

/// Which chrome bar an applied region belongs to — see this module's header
/// comment on why one slot per bar is enough.
///
/// `MenuBar` is only ever constructed by `chrome::windows` (Linux-only), so
/// the allow keeps the macOS dev-loop build quiet without hiding a real
/// unused variant on the platform that actually runs this code — the same
/// shape `home::render_dock` and friends already use for their own
/// layer-surface-only entry points.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegionSlot {
    MenuBar,
    Dock,
}

/// What a chrome bar's Wayland input region is set to. Mirrors the three
/// states `Window::set_input_region` distinguishes, so the cache can compare
/// wanted-vs-applied as one value instead of an `Option<Option<..>>`.
///
/// `Empty` is constructed only by `chrome::windows` (Linux-only) — same
/// reasoning as `RegionSlot`'s own allow just above.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum BarRegion {
    /// The whole surface receives input — Wayland's default, and what every
    /// surface this crate opens starts out as. `set_input_region(None)`.
    Full,
    /// No pointer or touch input at all; clicks fall straight through to
    /// whatever is beneath. `set_input_region(Some(&[]))`.
    Empty,
    /// Exactly one rectangle, in window coordinates.
    Rect(Bounds<Pixels>),
}

thread_local! {
    static MENU_BAR_APPLIED: Cell<Option<BarRegion>> = const { Cell::new(None) };
    static DOCK_APPLIED: Cell<Option<BarRegion>> = const { Cell::new(None) };
}

fn cache_for(slot: RegionSlot) -> &'static LocalKey<Cell<Option<BarRegion>>> {
    match slot {
        RegionSlot::MenuBar => &MENU_BAR_APPLIED,
        RegionSlot::Dock => &DOCK_APPLIED,
    }
}

/// Applies `wanted` to `window`'s surface, skipping the call entirely when
/// that is already what is applied — see this module's header comment on why
/// the skip matters.
///
/// A no-op on every platform whose `PlatformWindow` does not implement
/// `set_input_region` (its trait default is an empty body), which is every
/// platform except Wayland — so this is safe to call unconditionally from
/// cross-platform render code, including this crate's macOS dev loop.
pub(crate) fn apply(window: &Window, slot: RegionSlot, wanted: BarRegion) {
    let cache = cache_for(slot);
    if cache.with(Cell::get) == Some(wanted) {
        return;
    }
    match wanted {
        BarRegion::Full => window.set_input_region(None),
        BarRegion::Empty => window.set_input_region(Some(&[])),
        BarRegion::Rect(rect) => window.set_input_region(Some(&[rect])),
    }
    cache.with(|cell| cell.set(Some(wanted)));
    if crate::diag_enabled() {
        eprintln!("[chrome] input region for {slot:?} -> {wanted:?}");
    }
}

/// The smallest rectangle containing every rectangle in `rects`, or `None`
/// when there are none.
///
/// The dock's pill is the only child of its container, so in practice this
/// unions a single rect — but `on_children_prepainted` reports ALL children,
/// and a future round adding a second element next to the pill (a hover
/// label, a drag affordance) must widen the clickable region rather than
/// silently keep only the first child's.
pub(crate) fn union_of(rects: &[Bounds<Pixels>]) -> Option<Bounds<Pixels>> {
    let mut iter = rects.iter();
    let first = *iter.next()?;
    let (mut left, mut top) = (f32::from(first.origin.x), f32::from(first.origin.y));
    let (mut right, mut bottom) = (left + f32::from(first.size.width), top + f32::from(first.size.height));
    for rect in iter {
        let x = f32::from(rect.origin.x);
        let y = f32::from(rect.origin.y);
        left = left.min(x);
        top = top.min(y);
        right = right.max(x + f32::from(rect.size.width));
        bottom = bottom.max(y + f32::from(rect.size.height));
    }
    Some(Bounds { origin: point(px(left), px(top)), size: size(px(right - left), px(bottom - top)) })
}

/// Grows `rect` outward to whole pixels and clamps it to non-negative
/// coordinates.
///
/// gpui's Wayland backend converts each rectangle with a plain
/// `f32 as i32` cast, which truncates toward zero — a pill laid out at
/// `x = 385.5, width = 509.2` would become `x = 385, width = 509`, shaving
/// most of a pixel off the RIGHT edge while leaving the left edge alone.
/// Rounding outward here makes the region contain the painted pill in every
/// case instead of "usually"; at most it makes one border pixel on each side
/// clickable, which is the harmless direction to be wrong in.
pub(crate) fn snap_outward(rect: Bounds<Pixels>) -> Bounds<Pixels> {
    let left = f32::from(rect.origin.x).floor().max(0.);
    let top = f32::from(rect.origin.y).floor().max(0.);
    let right = (f32::from(rect.origin.x) + f32::from(rect.size.width)).ceil().max(left);
    let bottom = (f32::from(rect.origin.y) + f32::from(rect.size.height)).ceil().max(top);
    Bounds { origin: point(px(left), px(top)), size: size(px(right - left), px(bottom - top)) }
}

/// The region a chrome bar should claim while it is SHOWN, given the bounds
/// its container's children were actually laid out at.
///
/// `None`/an empty slice degrades to `BarRegion::Full` on purpose. A dock
/// container with no children cannot happen (the divider, the agent avatars
/// and the settings tile render even on a machine with nothing installed),
/// so this branch exists only to answer "what if" — and of the two possible
/// wrong answers, "the dock is still clickable, at the cost of the
/// pass-through" is the pre-fix behaviour this crate already shipped, while
/// "the dock claims nothing" would make the shell's only launcher row dead.
pub(crate) fn shown_region_for(children: &[Bounds<Pixels>]) -> BarRegion {
    match union_of(children) {
        Some(rect) => BarRegion::Rect(snap_outward(rect)),
        None => BarRegion::Full,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds { origin: point(px(x), px(y)), size: size(px(w), px(h)) }
    }

    #[test]
    fn union_of_nothing_is_none() {
        assert_eq!(union_of(&[]), None);
    }

    #[test]
    fn union_of_one_rect_is_that_rect() {
        assert_eq!(union_of(&[b(385.5, 0., 509., 66.)]), Some(b(385.5, 0., 509., 66.)));
    }

    #[test]
    fn union_of_several_rects_covers_all_of_them() {
        // Deliberately out of order and overlapping — the union must not
        // depend on which one is first.
        let got = union_of(&[b(100., 10., 50., 20.), b(80., 4., 10., 10.), b(120., 12., 60., 30.)]).unwrap();
        assert_eq!(got, b(80., 4., 100., 38.));
    }

    #[test]
    fn snap_outward_grows_a_fractional_rect_to_whole_pixels() {
        // The failure mode this guards: gpui's `f32 as i32` truncation would
        // turn (385.5, 509.2) into x=385 w=509, losing 0.7px off the right
        // edge. Snapped, the region covers 385..895 instead.
        let got = snap_outward(b(385.5, 0.25, 509.2, 65.5));
        assert_eq!(got, b(385., 0., 510., 66.));
    }

    #[test]
    fn snap_outward_leaves_an_already_whole_rect_alone() {
        assert_eq!(snap_outward(b(385., 0., 510., 66.)), b(385., 0., 510., 66.));
    }

    #[test]
    fn snap_outward_clamps_negative_origins_into_the_surface() {
        // A rect starting left of the surface keeps its right edge; only the
        // part inside the surface is claimed.
        let got = snap_outward(b(-4., -2., 20., 10.));
        assert_eq!(got, b(0., 0., 16., 8.));
    }

    #[test]
    fn shown_region_falls_back_to_the_whole_surface_when_nothing_was_laid_out() {
        assert_eq!(shown_region_for(&[]), BarRegion::Full);
    }

    #[test]
    fn shown_region_is_the_snapped_union_of_the_children() {
        assert_eq!(shown_region_for(&[b(385.5, 0., 509.2, 66.)]), BarRegion::Rect(b(385., 0., 510., 66.)));
    }

    /// The regression this whole module exists for, expressed as a bound on
    /// the numbers: a 1280-wide dock band whose pill is ~510 px wide must
    /// claim only the pill, leaving the ~770 px of transparent band either
    /// side of it clickable by the window underneath.
    #[test]
    fn the_dock_band_outside_the_pill_is_not_claimed() {
        let band_width = 1280.;
        let BarRegion::Rect(claimed) = shown_region_for(&[b(385., 0., 510., 66.)]) else {
            panic!("a laid-out pill must produce a rect region");
        };
        let claimed_width = f32::from(claimed.size.width);
        assert!(claimed_width < band_width, "the claimed region must be narrower than the surface");
        assert_eq!(band_width - claimed_width, 770., "everything outside the pill stays click-through");
        // And the ToS button's x on the appliance's own output — anywhere in
        // the left third of the band — is outside it.
        assert!(f32::from(claimed.origin.x) > 300.);
    }
}
