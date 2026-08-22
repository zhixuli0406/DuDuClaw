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

use smithay::{
    backend::renderer::element::{solid::SolidColorBuffer, solid::SolidColorRenderElement, Kind},
    utils::{Logical, Physical, Point, Scale},
};

/// Dimmed red while the agent seat is frozen — "can't move right now" is
/// legible at a glance without reading any log, matching DESIGN §3.4's
/// "系統級『共駕中』指示…不可隱藏" spirit at the cursor-overlay scale.
const AGENT_COLOR_FROZEN: [f32; 4] = [0.65, 0.12, 0.12, 0.85];
/// Brand amber while live. `pub(super)` (not private) since CD-1's
/// `highlight.rs` reuses the exact same color for the target highlight box
/// (task brief req 5: "顏色用 cursor.rs 的 `AGENT_COLOR_LIVE` 琥珀") — one
/// shared constant rather than a second copy that could drift.
pub(super) const AGENT_COLOR_LIVE: [f32; 4] = [1.0, 0.62, 0.0, 0.92];

/// Builds the agent-pointer cross reticle for this frame. `agent_pos` is the
/// agent seat's current pointer location (queried directly from
/// `PointerHandle::current_location()` — there's no need to track a duplicate
/// position field on `DuduclawComp`).
///
/// CUR-1: this used to build the human pointer as well; it no longer does.
/// See `crate::cursor` and this file's header.
pub fn build_agent_cursor_elements(
    agent_pos: Point<f64, Logical>,
    agent_frozen: bool,
) -> Vec<SolidColorRenderElement> {
    let scale = Scale::from(1.0);
    let mut elems = Vec::with_capacity(2);

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
