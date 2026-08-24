// CD-0 codrive spike — the AGENT cursor overlay.
// DESIGN-codrive-desktop-2026-08.md §3.3.2: "agent 游標畫成 compositor 內部
// render element（與人游標明確異形異色…）". Drawn with smithay's
// `SolidColorRenderElement` — a plain colored rectangle, zero texture/
// protocol cost — passed into `render_output`'s `custom_elements` slice
// alongside the window surfaces already rendered from `state.space`.
//
// The agent pointer is an amber cross/reticle built from two perpendicular
// rectangles, which reads as a distinct SILHOUETTE (not just a different
// color) using only the rectangle primitive `SolidColorRenderElement`
// offers. That is deliberate design, not a placeholder: a human glancing at
// the screen must never mistake an agent-driven pointer for their own.
//
// CUR-1 (2026-08-22) removed the HUMAN half of this file. CD-0 drew the
// human pointer here too, as a 10×10 pale square explicitly labelled a
// placeholder — which turned out to be invisible on a light background
// ("滑鼠是一個方塊，而非主流鼠標，而且還是白色的誰看得到"). The human pointer
// now lives in `crate::cursor`, which serves real XCursor theme artwork and
// honours what clients request. The agent cross intentionally did NOT move
// with it: it is compositor-owned by design and must ignore client requests.
//
// ── A2 共駕復活 (2026-08-24): the "ghost" cursor ──────────────────────────
// Two changes, both from the A2 wire contract §5:
//
// 1. **BEHAVIOR CHANGE — `DrivingMode::Human` now draws NOTHING.** Before
//    this round the agent cross was drawn on every frame unconditionally, so
//    a desktop with no co-drive session at all (or one that had just been
//    emergency-stopped) still carried an agent pointer sitting wherever the
//    last session left it. That is a lie in the most load-bearing possible
//    place: the whole purpose of this element is to tell a human "something
//    other than you can move a pointer right now". With no session, nothing
//    can, and the honest drawing is no drawing. `derive_mode` (`mode.rs`) is
//    what makes this expressible — `is_frozen()` alone could never tell
//    "frozen because a human touched it" from "frozen and there is no
//    session left".
// 2. **Ghost styling** — outline + translucency, instead of one flat opaque
//    cross. A dark halo 2 px larger on each side (alpha ≈0.35) keeps the
//    cross legible over both light and dark content without the compositor
//    knowing anything about what is underneath, and the core drops to alpha
//    ≈0.70 so the pointer reads as an overlay rather than as a normal cursor.
//    Still `SolidColorRenderElement` only — zero new dependencies.

use smithay::{
    backend::renderer::element::{solid::SolidColorBuffer, solid::SolidColorRenderElement, Kind},
    utils::{Logical, Physical, Point, Scale},
};

use super::mode::DrivingMode;

/// Dimmed red while the human holds the wheel ([`DrivingMode::Handover`]) —
/// "the agent can't move right now" is legible at a glance without reading
/// any log, matching DESIGN §3.4's "系統級『共駕中』指示…不可隱藏" spirit at
/// the cursor-overlay scale. `pub(super)` since A2's `mode_indicator.rs`
/// reuses the same pair — one shared definition rather than a second copy
/// that could drift.
pub(super) const AGENT_COLOR_FROZEN: [f32; 4] = [0.65, 0.12, 0.12, 0.85];
/// Brand amber while the agent is driving ([`DrivingMode::CoDrive`]).
/// `pub(super)` (not private) since CD-1's `highlight.rs` reuses the exact
/// same color for the target highlight box (task brief req 5: "顏色用
/// cursor.rs 的 `AGENT_COLOR_LIVE` 琥珀") — one shared constant rather than a
/// second copy that could drift. A2's `mode_indicator.rs` reuses it too.
pub(super) const AGENT_COLOR_LIVE: [f32; 4] = [1.0, 0.62, 0.0, 0.92];

/// A2: the "ghost" halo drawn behind the cross. Near-black and mostly
/// transparent, so it reads as a soft outline on light content and simply
/// disappears into dark content — the cross's own colour carries the state,
/// the halo only carries the edge.
const GHOST_OUTLINE_COLOR: [f32; 4] = [0.04, 0.04, 0.04, 0.35];
/// A2: the core cross's alpha. Lower than the pre-A2 0.85/0.92 so the
/// pointer reads as a compositor overlay rather than as a solid object the
/// human could try to click.
const GHOST_CORE_ALPHA: f32 = 0.70;
/// How much larger the halo is than the core, per side.
const GHOST_OUTLINE_PX: f64 = 2.0;

/// Core cross geometry, `(offset_x, offset_y, width, height)` in logical
/// pixels relative to the pointer hotspot. Two perpendicular bars.
const CORE_BARS: [(f64, f64, i32, i32); 2] = [(-9.0, -2.0, 18, 4), (-2.0, -9.0, 4, 18)];

/// Builds the agent-pointer ghost reticle for this frame. `agent_pos` is the
/// agent seat's current pointer location (queried directly from
/// `PointerHandle::current_location()` — there's no need to track a duplicate
/// position field on `DuduclawComp`).
///
/// `mode` replaced A2-era `agent_frozen: bool`. See this file's header for
/// the behavior change that came with it: [`DrivingMode::Human`] draws
/// nothing.
///
/// CUR-1: this used to build the human pointer as well; it no longer does.
/// See `crate::cursor` and this file's header.
///
/// `scale` is the rendered output's own live scale
/// (`render::output_render_scale`) — WP-comp-shell-display D4b-3 replaced a
/// hardcoded `Scale::from(1.0)`. Unlike the human cursor, this element is
/// `SolidColorRenderElement`-based, whose `Element::geometry()` IGNORES the
/// scale `render_output` passes it at render time (checked against smithay
/// 0.7.0 source: `fn geometry(&self, _scale: Scale<f64>)`) — so both the
/// cross's PHYSICAL position and its PHYSICAL size are baked in right here,
/// at construction, from whatever `scale` this caller supplies. Passing the
/// wrong value does not just misplace the cross, it also mis-sizes it.
pub fn build_agent_cursor_elements(
    agent_pos: Point<f64, Logical>,
    mode: DrivingMode,
    scale: Scale<f64>,
) -> Vec<SolidColorRenderElement> {
    let core_color = match mode {
        // No session — nothing may move this pointer, so there is no pointer
        // to draw. See this file's header (A2 behavior change #1).
        DrivingMode::Human => return Vec::new(),
        DrivingMode::CoDrive => AGENT_COLOR_LIVE,
        DrivingMode::Handover => AGENT_COLOR_FROZEN,
    };
    let core_color = [core_color[0], core_color[1], core_color[2], GHOST_CORE_ALPHA];

    let mut elems = Vec::with_capacity(4);

    // The core goes in FIRST on purpose: this crate's backends treat earlier
    // elements in the `custom_elements` slice as nearer the viewer (see
    // `winit_backend.rs`'s "the HUMAN cursor is built first" comment), and the
    // halo fully contains the core — pushed first, the halo would hide the
    // very cross it exists to outline.
    for (dx, dy, w, h) in CORE_BARS {
        elems.push(bar(agent_pos, dx, dy, w, h, core_color, scale));
    }
    let o = GHOST_OUTLINE_PX;
    for (dx, dy, w, h) in CORE_BARS {
        elems.push(bar(
            agent_pos,
            dx - o,
            dy - o,
            w + (o as i32) * 2,
            h + (o as i32) * 2,
            GHOST_OUTLINE_COLOR,
            scale,
        ));
    }

    elems
}

fn bar(
    pos: Point<f64, Logical>,
    dx: f64,
    dy: f64,
    w: i32,
    h: i32,
    color: [f32; 4],
    scale: Scale<f64>,
) -> SolidColorRenderElement {
    let buf = SolidColorBuffer::new((w, h), color);
    let loc: Point<i32, Physical> =
        (pos + Point::<f64, Logical>::from((dx, dy))).to_physical_precise_round(scale);
    SolidColorRenderElement::from_buffer(&buf, loc, scale, 1.0, Kind::Cursor)
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only needed to call `.geometry()` directly in the D4b-3 scale
    // regression test below — everything else in this file builds elements
    // without ever inspecting their own trait-level geometry.
    use smithay::backend::renderer::element::Element;

    #[test]
    fn human_mode_draws_no_agent_pointer_at_all() {
        // A2 behavior change: before this round the cross was drawn
        // unconditionally, leaving a ghost pointer on a desktop with no
        // co-drive session. See this file's header.
        let elems = build_agent_cursor_elements(Point::from((100.0, 100.0)), DrivingMode::Human, Scale::from(1.0));
        assert!(elems.is_empty());
    }

    #[test]
    fn the_two_active_modes_draw_a_core_cross_plus_its_halo() {
        for mode in [DrivingMode::CoDrive, DrivingMode::Handover] {
            let elems = build_agent_cursor_elements(Point::from((0.0, 0.0)), mode, Scale::from(1.0));
            assert_eq!(elems.len(), 4, "{mode:?}: two core bars + two halo bars");
        }
    }

    /// WP-comp-shell-display D4b-3: since `SolidColorRenderElement`'s size is
    /// baked in at construction (its `geometry()` ignores the render-time
    /// scale — see this function's own doc), a caller-supplied scale must
    /// actually change the buffer this produces, or the "single source of
    /// truth" claim is untested.
    #[test]
    fn a_real_output_scale_changes_the_baked_geometry() {
        let at_1x = build_agent_cursor_elements(Point::from((100.0, 100.0)), DrivingMode::CoDrive, Scale::from(1.0));
        let at_2x = build_agent_cursor_elements(Point::from((100.0, 100.0)), DrivingMode::CoDrive, Scale::from(2.0));
        assert_eq!(at_1x.len(), at_2x.len());
        for (a, b) in at_1x.iter().zip(at_2x.iter()) {
            assert_ne!(
                a.geometry(Scale::from(1.0)),
                b.geometry(Scale::from(1.0)),
                "the 2x-scale element must not report the same physical geometry as the 1x one"
            );
        }
    }

    #[test]
    fn the_core_cross_is_two_perpendicular_bars() {
        let [(_, _, w0, h0), (_, _, w1, h1)] = CORE_BARS;
        assert!(w0 > h0, "the first bar must be horizontal");
        assert!(h1 > w1, "the second bar must be vertical");
    }

    #[test]
    fn the_halo_is_strictly_larger_than_the_core_on_every_side() {
        const { assert!(GHOST_OUTLINE_PX > 0.0) };
        for (dx, dy, w, h) in CORE_BARS {
            let (hx, hy) = (dx - GHOST_OUTLINE_PX, dy - GHOST_OUTLINE_PX);
            let (hw, hh) = (w + (GHOST_OUTLINE_PX as i32) * 2, h + (GHOST_OUTLINE_PX as i32) * 2);
            assert!(hx < dx && hy < dy, "the halo must start above/left of the core");
            assert!(
                hx + hw as f64 > dx + w as f64 && hy + hh as f64 > dy + h as f64,
                "the halo must end below/right of the core"
            );
        }
    }

    #[test]
    fn the_ghost_is_translucent_and_the_halo_is_the_fainter_layer() {
        // `const` blocks: these are design invariants over compile-time
        // constants, so they should fail the BUILD, not a test run.
        const { assert!(GHOST_CORE_ALPHA < 1.0, "a ghost pointer must not be opaque") };
        const {
            assert!(
                GHOST_OUTLINE_COLOR[3] < GHOST_CORE_ALPHA,
                "the halo must be fainter than the cross it outlines"
            )
        };
    }

    #[test]
    fn codrive_and_handover_are_visually_distinguishable() {
        // Not just "a different alpha" — the RGB has to differ, or a glance
        // cannot tell "the agent is driving" from "I hold the wheel".
        assert_ne!(AGENT_COLOR_LIVE[..3], AGENT_COLOR_FROZEN[..3]);
    }
}
