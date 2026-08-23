//! WM-3: the two pure decisions layer-shell brings with it — **where a layer
//! surface sits in the z-order**, and **how much of the output is left for
//! ordinary windows**.
//!
//! Both are plain functions over plain values, per this crate's standing split
//! ("pure logic unit-tested, live `Space`/`Seat`/`LayerMap` state live-run
//! verified" — see `codrive/window_target.rs`'s module doc for the original
//! statement of that rule).

use smithay::{
    utils::{Logical, Point, Rectangle, Size},
    wayland::shell::wlr_layer::{KeyboardInteractivity, Layer},
};

use crate::window_policy::{work_area, ReservedBands, MIN_APP_HEIGHT};

/// Front-to-back render rank. **Lower is nearer the viewer**, matching the
/// order `OutputDamageTracker::render_output` consumes its element slice in
/// (earlier elements are on top — see `decor::paint`'s module doc).
///
/// The five bands are the wlr-layer-shell spec's own stacking order with the
/// ordinary window stack slotted where the protocol says it goes: *"Traditional
/// shell surfaces will typically be rendered between the bottom and top
/// layers."*
pub const RANK_OVERLAY: u8 = 0;
/// See [`RANK_OVERLAY`].
pub const RANK_TOP: u8 = 1;
/// The ordinary `xdg_toplevel` stack. See [`RANK_OVERLAY`].
pub const RANK_WINDOWS: u8 = 2;
/// See [`RANK_OVERLAY`].
pub const RANK_BOTTOM: u8 = 3;
/// See [`RANK_OVERLAY`].
pub const RANK_BACKGROUND: u8 = 4;

/// The render rank of one layer.
///
/// smithay's own `space_render_elements` only splits layers into two groups
/// (`Background | Bottom` below the windows, `Top | Overlay` above) and does
/// **not** order `Overlay` above `Top` within the upper group — it emits them
/// in reverse map-insertion order instead. That is a real difference, not a
/// stylistic one: a lock screen or a global palette on `Overlay` must cover a
/// panel on `Top` regardless of which mapped first. This crate therefore ranks
/// all four bands explicitly and renders group by group.
pub fn layer_rank(layer: Layer) -> u8 {
    match layer {
        Layer::Overlay => RANK_OVERLAY,
        Layer::Top => RANK_TOP,
        Layer::Bottom => RANK_BOTTOM,
        Layer::Background => RANK_BACKGROUND,
    }
}

/// Is this layer drawn **above** the ordinary window stack?
///
/// The same predicate decides render order and pointer routing, which is the
/// point of having it in one place: a surface that draws over a window must
/// also receive the clicks that land on it.
pub fn is_above_windows(layer: Layer) -> bool {
    layer_rank(layer) < RANK_WINDOWS
}

/// The four layers in front-to-back order — the iteration order both the
/// renderer and the pointer router walk.
pub const LAYERS_FRONT_TO_BACK: [Layer; 4] = [
    Layer::Overlay,
    Layer::Top,
    Layer::Bottom,
    Layer::Background,
];

/// The protocol's keyboard-focus rule for layer surfaces, as a pure decision
/// over an already-ordered (front-to-back) candidate list.
///
/// `zwlr_layer_shell_v1` is explicit about this and it is **not** a
/// "compositor policy" choice:
///
/// > `exclusive`: … For the top and overlay layers, the seat will always give
/// > exclusive keyboard focus to the top-most layer which has exclusive
/// > keyboard interactivity set.
///
/// D9-bug2 (2026-08-23): comp did not implement that sentence at all, and the
/// whole "Launcher search box will not accept typing" report is downstream of
/// it. The shell's ⌘K overlay is created with `KeyboardInteractivity::
/// Exclusive` precisely so that opening it from the keyboard lands the caret in
/// its search box — but comp only ever moved keyboard focus to a layer surface
/// when the surface was *clicked*, or when nothing held focus at all. Opening
/// the palette with Super+K therefore left keyboard focus on the desktop
/// surface, in a **different gpui window**, so every keystroke was delivered to
/// the wrong surface and the search field (correctly focused inside its own
/// window) never saw a single one.
///
/// Only `Top`/`Overlay` are eligible, exactly as quoted: an exclusive request
/// from `bottom`/`background` is deliberately ignored here (the spec leaves it
/// to the compositor, and honouring it would let a wallpaper client hold the
/// keyboard hostage).
///
/// Returns the index into `candidates` — indices, not surfaces, so this stays a
/// plain data function with no smithay `Space`/`LayerMap` state in it (the
/// crate's standing pure-vs-live split, see this module's own doc comment).
pub fn topmost_exclusive(candidates: &[(Layer, KeyboardInteractivity)]) -> Option<usize> {
    candidates
        .iter()
        .position(|(layer, interactivity)| {
            is_above_windows(*layer) && matches!(interactivity, KeyboardInteractivity::Exclusive)
        })
}

/// The fallback used when no exclusive claim exists: the front-most surface
/// that is willing to take keyboard focus at all. Same ordering contract as
/// [`topmost_exclusive`] — `candidates` is already front-to-back.
///
/// Unlike the exclusive rule this one accepts every layer, including
/// `background`: the shell's own desktop surface lives there and is the
/// legitimate keyboard target on an otherwise empty screen.
pub fn topmost_focusable(candidates: &[(Layer, KeyboardInteractivity)]) -> Option<usize> {
    candidates.iter().position(|(_, interactivity)| {
        matches!(
            interactivity,
            KeyboardInteractivity::Exclusive | KeyboardInteractivity::OnDemand
        )
    })
}

/// The part of `output` an ordinary application window may occupy, given what
/// layer surfaces have claimed.
///
/// `zone` is the layer map's `non_exclusive_zone()` in **output-local**
/// coordinates (that is what smithay's `LayerMap` computes — it arranges
/// against `Rectangle::from_size(mode)`, origin `(0, 0)`), or `None` when no
/// layer map exists for this output yet.
///
/// ## The transition rule: **intersection**, and why not "replacement"
///
/// `duduclaw-shell` paints its menu bar and dock *inside its own full-output
/// toplevel* today; nothing claims an exclusive zone. WM-1's hard-coded
/// [`ReservedBands`] exist for exactly that arrangement. So the answer is the
/// banded work area **intersected with** the layer map's leftover zone:
///
/// * nothing claimed anything (the zone is the whole output) ⇒ the intersection
///   *is* the banded area, i.e. **byte-identical to WM-2**;
/// * a layer surface claims space ⇒ the work area shrinks by that too.
///
/// The first draft of this function made the zone **replace** the bands, which
/// reads more naturally from "exclusive zone 取代 hardcode reserved band" — and
/// the very first live run showed why it is wrong. Bringing up `waybar`
/// (a 30 px top panel) against the unmigrated shell moved every window's work
/// area from `(0, 30, 1280, 680)` to `(0, 30, 1280, 770)`: the panel's own 30 px
/// claim was honoured and **the shell's 90 px dock reservation silently
/// vanished**, so windows would have been placed straight over the dock. Any
/// third-party layer client would have done that. Intersection cannot: a layer
/// surface may only ever shrink the work area further.
///
/// Double-counting is the theoretical cost, and it does not bite in practice:
/// when the shell migrates, its layer surfaces claim *the same* 30 px and 90 px
/// the constants used to describe, and `A ∩ A = A`. The migration package should
/// still zero the constants (they document the shell's own chrome — see
/// [`ReservedBands`]) so the two cannot drift apart later.
///
/// **Done, WM-3 (2026-08-23):** [`ReservedBands::default()`] is now `{top: 0,
/// bottom: 0}` — see `window_policy`'s own module doc for the migration and
/// [`window_policy::RESERVED_TOP_ENV`]/[`window_policy::RESERVED_BOTTOM_ENV`]
/// for the compatibility fallback that keeps this file's intersection tests
/// meaningful (they pass **explicit** non-zero bands rather than relying on a
/// default that no longer reserves anything).
///
/// A zone that would leave less than [`MIN_APP_HEIGHT`] falls back to the
/// banded area rather than to a sliver, for the same reason [`work_area`]
/// falls back to the whole output: a nonsense claim must never produce a window
/// a client cannot render into.
pub fn effective_work_area(
    output: Rectangle<i32, Logical>,
    zone: Option<Rectangle<i32, Logical>>,
    bands: ReservedBands,
) -> Rectangle<i32, Logical> {
    let banded = work_area(output, bands);
    let Some(zone) = zone else {
        return banded;
    };
    if output.size.w <= 0 || output.size.h <= 0 {
        return output;
    }
    // The zone is output-local; the banded area is global.
    let zone_global = Rectangle::new(
        Point::from((output.loc.x + zone.loc.x, output.loc.y + zone.loc.y)),
        Size::from((zone.size.w.max(0), zone.size.h.max(0))),
    );
    let Some(intersection) = banded.intersection(zone_global) else {
        return banded;
    };
    if intersection.size.w <= 0 || intersection.size.h < MIN_APP_HEIGHT.min(banded.size.h) {
        return banded;
    }
    intersection
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((x, y)), Size::from((w, h)))
    }

    #[test]
    fn the_z_order_is_background_bottom_windows_top_overlay() {
        // The whole point of this work package's first item, asserted as one
        // chain: nearer the viewer means a strictly smaller rank. Written as a
        // sorted-sequence check rather than four `assert!(A < B)` lines,
        // because clippy (rightly) refuses assertions over compile-time
        // constants — and the sequence form is what the requirement actually
        // says anyway: "background < bottom < 一般視窗 < top < overlay".
        let front_to_back = [
            RANK_OVERLAY,
            RANK_TOP,
            RANK_WINDOWS,
            RANK_BOTTOM,
            RANK_BACKGROUND,
        ];
        assert!(
            front_to_back.windows(2).all(|w| w[0] < w[1]),
            "ranks must strictly increase away from the viewer: {front_to_back:?}"
        );
        assert_eq!(layer_rank(Layer::Overlay), RANK_OVERLAY);
        assert_eq!(layer_rank(Layer::Top), RANK_TOP);
        assert_eq!(layer_rank(Layer::Bottom), RANK_BOTTOM);
        assert_eq!(layer_rank(Layer::Background), RANK_BACKGROUND);
    }

    #[test]
    fn overlay_outranks_top_which_upstream_does_not_guarantee() {
        // smithay's own `space_render_elements` lumps these two together; a
        // lock screen over a panel depends on this being ordered.
        assert!(layer_rank(Layer::Overlay) < layer_rank(Layer::Top));
    }

    #[test]
    fn only_top_and_overlay_sit_above_the_window_stack() {
        assert!(is_above_windows(Layer::Overlay));
        assert!(is_above_windows(Layer::Top));
        assert!(!is_above_windows(Layer::Bottom));
        assert!(!is_above_windows(Layer::Background));
    }

    #[test]
    fn the_front_to_back_walk_visits_every_layer_exactly_once_in_rank_order() {
        let ranks: Vec<u8> = LAYERS_FRONT_TO_BACK.iter().copied().map(layer_rank).collect();
        assert_eq!(ranks, vec![RANK_OVERLAY, RANK_TOP, RANK_BOTTOM, RANK_BACKGROUND]);
    }

    #[test]
    fn no_layer_map_at_all_keeps_the_hard_coded_bands() {
        // The pre-WM-3 behaviour, byte for byte.
        let out = rect(0, 0, 1280, 800);
        assert_eq!(
            effective_work_area(out, None, ReservedBands::default()),
            work_area(out, ReservedBands::default())
        );
    }

    #[test]
    fn a_zone_equal_to_the_whole_output_means_nothing_claimed() {
        // Explicit, non-zero bands here (rather than `ReservedBands::default()`,
        // which WM-3 zeroed — see `window_policy`'s module doc): this test is
        // about the intersection rule ("an unclaimed zone falls back to the
        // banded area"), not about the production default, so it stands in for
        // a legacy shell that still configures DUDUCLAW_COMP_RESERVED_TOP/
        // _BOTTOM instead of migrating to layer-shell.
        let bands = ReservedBands { top: 30, bottom: 90 };
        let out = rect(0, 0, 1280, 800);
        let untouched = rect(0, 0, 1280, 800);
        let got = effective_work_area(out, Some(untouched), bands);
        assert_eq!(got, work_area(out, bands));
        assert_eq!((got.loc.y, got.size.h), (30, 680));
    }

    #[test]
    fn a_migrated_shells_own_zones_do_not_double_count_against_a_legacy_bands_override() {
        // A migrated shell: 30px menu bar on top, 90px dock at the bottom, both
        // as exclusive zones. Intersection of two identical rectangles is that
        // rectangle — 680px, NOT 680 - 120 — even when a legacy operator
        // override still sets non-zero bands (WM-3's production DEFAULT is
        // zero, in which case this is trivially true; this explicit-bands
        // version is what proves the INTERSECTION rule itself, not just the
        // zeroed default).
        let bands = ReservedBands { top: 30, bottom: 90 };
        let out = rect(0, 0, 1280, 800);
        let zone = rect(0, 30, 1280, 680);
        let got = effective_work_area(out, Some(zone), bands);
        assert_eq!((got.loc.x, got.loc.y), (0, 30));
        assert_eq!((got.size.w, got.size.h), (1280, 680));
    }

    #[test]
    fn a_third_party_panel_can_only_shrink_the_work_area_never_un_reserve_the_dock() {
        // THE live-run regression this rule exists for (WM-3, 2026-08-23):
        // `waybar` claiming 30px at the top used to REPLACE the bands, which
        // silently gave back the shell's 90px dock reservation and would have
        // placed windows straight over the dock. Explicit bands, standing in
        // for a legacy (unmigrated) shell — WM-3 zeroed the production
        // default, so this scenario only still arises via the
        // DUDUCLAW_COMP_RESERVED_TOP/_BOTTOM compatibility override.
        let bands = ReservedBands { top: 30, bottom: 90 };
        let out = rect(0, 0, 1280, 800);
        let waybar_zone = rect(0, 30, 1280, 770);
        let got = effective_work_area(out, Some(waybar_zone), bands);
        assert_eq!(
            (got.loc.y, got.size.h),
            (30, 680),
            "the dock's 90px reservation must survive a third-party panel"
        );
    }

    #[test]
    fn a_panel_below_the_menu_bar_shrinks_the_work_area_further() {
        // A second 40px panel under the shell's own menu bar: 30 + 40 = 70 gone
        // at the top, dock unchanged. Explicit bands — see the tests above for
        // why `ReservedBands::default()` (now zero) would not exercise this.
        let bands = ReservedBands { top: 30, bottom: 90 };
        let out = rect(0, 0, 1280, 800);
        let got = effective_work_area(out, Some(rect(0, 70, 1280, 730)), bands);
        assert_eq!((got.loc.y, got.size.h), (70, 640));
    }

    #[test]
    fn the_zone_is_output_local_and_is_translated_onto_the_outputs_origin() {
        // udev maps additional connectors side by side at (w, 0); a LayerMap
        // always arranges from (0, 0). Explicit bands — see the tests above.
        let bands = ReservedBands { top: 30, bottom: 90 };
        let out = rect(1280, 0, 1920, 1080);
        let zone = rect(0, 40, 1920, 1040);
        let got = effective_work_area(out, Some(zone), bands);
        assert_eq!(got.loc.x, 1280, "the zone must land on THIS output, not the first one");
        assert_eq!(got.loc.y, 40, "40 > the 30px band, so the zone wins on the top edge");
        assert_eq!(got.size.h, 1080 - 40 - 90);
    }

    #[test]
    fn a_left_anchored_exclusive_zone_shifts_the_work_areas_origin() {
        let bands = ReservedBands { top: 30, bottom: 90 };
        let out = rect(0, 0, 1280, 800);
        let zone = rect(64, 0, 1216, 800);
        let got = effective_work_area(out, Some(zone), bands);
        assert_eq!((got.loc.x, got.loc.y), (64, 30));
        assert_eq!((got.size.w, got.size.h), (1216, 680));
    }

    #[test]
    fn a_zone_that_claims_almost_everything_falls_back_to_the_banded_area() {
        // A panel that claims 780 of 800 pixels would leave a 20px sliver — an
        // ignored claim is the lesser evil, exactly as `work_area` already
        // decides for an absurd band.
        let out = rect(0, 0, 1280, 800);
        let zone = rect(0, 780, 1280, 20);
        assert_eq!(
            effective_work_area(out, Some(zone), ReservedBands::default()),
            work_area(out, ReservedBands::default())
        );
    }

    #[test]
    fn a_zone_claiming_the_entire_output_falls_back_rather_than_going_degenerate() {
        // `LayerMap::arrange` sets the zone to 0x0 for `Anchor::all()` +
        // exclusive. A zero-sized configure is a protocol hazard, not a layout.
        let out = rect(0, 0, 1280, 800);
        assert_eq!(
            effective_work_area(out, Some(rect(0, 0, 0, 0)), ReservedBands::default()),
            work_area(out, ReservedBands::default())
        );
    }

    #[test]
    fn a_zone_that_does_not_overlap_the_banded_area_at_all_is_ignored() {
        // Pathological, but reachable via a bad `arrange` result: a zone
        // entirely inside the menu-bar band. Falling back beats returning a
        // rectangle with no pixels in it.
        let out = rect(0, 0, 1280, 800);
        assert_eq!(
            effective_work_area(out, Some(rect(0, 0, 1280, 20)), ReservedBands::default()),
            work_area(out, ReservedBands::default())
        );
    }

    #[test]
    fn a_degenerate_output_is_returned_unchanged_whatever_the_zone_says() {
        let zero = rect(0, 0, 0, 0);
        assert_eq!(effective_work_area(zero, Some(rect(0, 0, 10, 10)), ReservedBands::default()), zero);
    }

    // ── D9-bug2: the keyboard-focus rule ────────────────────────────────────
    // The candidate list is always front-to-back, which is what makes
    // "top-most" mean `position(...)` (the FIRST match) in both functions.

    /// The shell's real layout the moment ⌘K opens the palette: overlay
    /// (exclusive) over menu bar + dock (none) over the desktop (on-demand).
    fn shell_with_palette() -> Vec<(Layer, KeyboardInteractivity)> {
        vec![
            (Layer::Overlay, KeyboardInteractivity::Exclusive),
            (Layer::Top, KeyboardInteractivity::None),
            (Layer::Top, KeyboardInteractivity::None),
            (Layer::Background, KeyboardInteractivity::OnDemand),
        ]
    }

    /// The same shell one frame after Escape: the palette is gone.
    fn shell_without_palette() -> Vec<(Layer, KeyboardInteractivity)> {
        vec![
            (Layer::Top, KeyboardInteractivity::None),
            (Layer::Top, KeyboardInteractivity::None),
            (Layer::Background, KeyboardInteractivity::OnDemand),
        ]
    }

    #[test]
    fn an_exclusive_overlay_claims_the_keyboard_over_every_non_exclusive_surface() {
        assert_eq!(topmost_exclusive(&shell_with_palette()), Some(0));
    }

    #[test]
    fn without_an_exclusive_claim_nothing_is_forced_and_the_live_holder_keeps_focus() {
        assert_eq!(topmost_exclusive(&shell_without_palette()), None);
    }

    #[test]
    fn an_exclusive_claim_from_bottom_or_background_is_ignored() {
        // The spec grants the always-wins rule to the top and overlay layers
        // only. A wallpaper or desktop-icon client asking for `exclusive` must
        // not be able to hold the keyboard hostage.
        let candidates = vec![
            (Layer::Background, KeyboardInteractivity::Exclusive),
            (Layer::Bottom, KeyboardInteractivity::Exclusive),
        ];
        assert_eq!(topmost_exclusive(&candidates), None);
        // …but it is still a perfectly good FALLBACK target once focus is
        // genuinely vacant.
        assert_eq!(topmost_focusable(&candidates), Some(0));
    }

    #[test]
    fn two_exclusive_claimants_resolve_to_the_front_most_one() {
        let candidates = vec![
            (Layer::Overlay, KeyboardInteractivity::Exclusive),
            (Layer::Top, KeyboardInteractivity::Exclusive),
        ];
        assert_eq!(topmost_exclusive(&candidates), Some(0));
    }

    #[test]
    fn the_fallback_skips_surfaces_that_refused_the_keyboard() {
        // The menu bar and dock are `None`: after the palette closes, focus
        // must land on the desktop surface, never on a bar that cannot use it.
        assert_eq!(topmost_focusable(&shell_without_palette()), Some(2));
    }

    #[test]
    fn an_empty_screen_yields_no_target_at_all_rather_than_a_default() {
        assert_eq!(topmost_exclusive(&[]), None);
        assert_eq!(topmost_focusable(&[]), None);
    }
}
