// CD-0 codrive spike — agent + human cursor overlays.
// DESIGN-codrive-desktop-2026-08.md §3.3.2: "agent 游標畫成 compositor 內部
// render element（與人游標明確異形異色…）". Both cursors are drawn with
// smithay's `SolidColorRenderElement` — a plain colored rectangle, zero
// texture/protocol cost — passed into `render_output`'s `custom_elements`
// slice alongside the window surfaces already rendered from `state.space`.
//
// Placeholder geometry only ("純色方塊/簡單形狀即可，不用美術資產" per the
// task brief): human = a small pale square (the plain default pointer
// shape); agent = an amber cross/reticle built from two perpendicular
// rectangles, which reads as a distinct SILHOUETTE (not just a different
// color) using only the rectangle primitive `SolidColorRenderElement`
// offers. Real brand art (hand-drawn SVG claw cursor, per the shell emoji
// convention noted in DESIGN §3.3.2) is deferred past CD-0.

use smithay::{
    backend::renderer::element::{solid::SolidColorBuffer, solid::SolidColorRenderElement, Kind},
    utils::{Logical, Physical, Point, Scale},
};

const HUMAN_COLOR: [f32; 4] = [0.95, 0.95, 0.95, 0.95];
/// Dimmed red while the agent seat is frozen — "can't move right now" is
/// legible at a glance without reading any log, matching DESIGN §3.4's
/// "系統級『共駕中』指示…不可隱藏" spirit at the cursor-overlay scale.
const AGENT_COLOR_FROZEN: [f32; 4] = [0.65, 0.12, 0.12, 0.85];
/// Brand amber while live. `pub(super)` (not private) since CD-1's
/// `highlight.rs` reuses the exact same color for the target highlight box
/// (task brief req 5: "顏色用 cursor.rs 的 `AGENT_COLOR_LIVE` 琥珀") — one
/// shared constant rather than a second copy that could drift.
pub(super) const AGENT_COLOR_LIVE: [f32; 4] = [1.0, 0.62, 0.0, 0.92];

/// Builds the human-pointer square and the agent-pointer cross reticle for
/// this frame. `human_pos`/`agent_pos` are each seat's current pointer
/// location (queried directly from `PointerHandle::current_location()` —
/// there's no need to track a duplicate position field on `DuduclawComp`).
pub fn build_cursor_elements(
    human_pos: Point<f64, Logical>,
    agent_pos: Point<f64, Logical>,
    agent_frozen: bool,
) -> Vec<SolidColorRenderElement> {
    let scale = Scale::from(1.0);
    let mut elems = Vec::with_capacity(3);

    let human_buf = SolidColorBuffer::new((10, 10), HUMAN_COLOR);
    let human_loc: Point<i32, Physical> =
        (human_pos + Point::<f64, Logical>::from((-5.0, -5.0))).to_physical_precise_round(scale);
    elems.push(SolidColorRenderElement::from_buffer(
        &human_buf,
        human_loc,
        scale,
        1.0,
        Kind::Cursor,
    ));

    let color = if agent_frozen { AGENT_COLOR_FROZEN } else { AGENT_COLOR_LIVE };
    let h_bar = SolidColorBuffer::new((18, 4), color);
    let v_bar = SolidColorBuffer::new((4, 18), color);
    let h_loc: Point<i32, Physical> =
        (agent_pos + Point::<f64, Logical>::from((-9.0, -2.0))).to_physical_precise_round(scale);
    let v_loc: Point<i32, Physical> =
        (agent_pos + Point::<f64, Logical>::from((-2.0, -9.0))).to_physical_precise_round(scale);
    elems.push(SolidColorRenderElement::from_buffer(&h_bar, h_loc, scale, 1.0, Kind::Cursor));
    elems.push(SolidColorRenderElement::from_buffer(&v_bar, v_loc, scale, 1.0, Kind::Cursor));

    elems
}
