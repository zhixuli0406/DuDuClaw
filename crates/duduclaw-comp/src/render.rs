//! Render elements shared by every backend.
//!
//! Extracted from `winit_backend.rs` in the A4-1 round (udev/DRM backend)
//! purely so the two backends can share one definition instead of the udev
//! backend having to `use crate::winit_backend::CodriveElement` — which
//! would make "the real-hardware backend depends on the nested development
//! backend" true at the module-graph level, which it isn't. The macro
//! invocation itself is unchanged from the CD-2 shadow-workspace round; see
//! the doc comment below (moved here verbatim) for why it exists at all.

use smithay::{
    backend::renderer::{
        element::{
            memory::MemoryRenderBufferRenderElement, render_elements,
            solid::SolidColorRenderElement, surface::WaylandSurfaceRenderElement,
            texture::TextureRenderElement,
        },
        gles::{GlesRenderer, GlesTexture},
    },
    output::Output,
    utils::Scale,
};

/// WP-comp-shell-display D4b-3: the single source of truth for the geometry
/// scale used when building EVERY custom render element for one output's
/// frame — the human cursor (`cursor::build_human_cursor_elements`), the
/// agent cursor (`codrive::build_agent_cursor_elements`), the target
/// highlight box (`codrive::codrive_highlight_elements_at`), and the
/// screen-edge co-drive indicator (`codrive::build_mode_indicator_elements`).
///
/// Before this round each of those four call sites hardcoded
/// `Scale::from(1.0)` independently — only `decor::paint::
/// build_output_elements` (windows/layer-shell/switcher/IME) read the
/// output's REAL scale. That meant a live `set_output_scale` would move
/// decorations while leaving every cursor/highlight/indicator pixel
/// positioned and sized as if scale was still 1.0 — a real desync, not a
/// hypothetical one (see `shell_control/mod.rs`'s "Scale, real as of D4b-3"
/// section for the full before/after).
///
/// This is deliberately a free function taking `&Output`, not a cached
/// field: `Output::current_scale()` is already O(1) (an `Arc<Mutex<..>>`
/// read), and a cache would be one more thing to invalidate the moment
/// `set_output_scale` changes it live. Every caller reads the SAME live
/// value at the SAME point in one frame's construction — `decor::paint::
/// build_output_elements` calls this too (previously it computed the
/// identical expression inline), so a window's decoration and the cursor
/// drawn a few lines earlier in the same frame can never disagree.
pub(crate) fn output_render_scale(output: &Output) -> Scale<f64> {
    Scale::from(output.current_scale().fractional_scale())
}

// CD-2 shadow workspace (WP-CD2-shadow, DESIGN §3.3.4): the same
// "compositor-internal render element" convention `codrive/cursor.rs` and
// `codrive/highlight.rs` already use for the two cursors and the target
// highlight box (both zero-texture `SolidColorRenderElement`s), extended
// with a real sampled texture for the PiP thumbnail
// (`DuduclawComp::codrive_render_pip`, `codrive/shadow.rs`). smithay's
// `render_elements!` macro (used the same way anvil-class compositors
// combine heterogeneous custom-element types) generates the `Element`/
// `RenderElement<GlesRenderer>` glue for this enum so `render_output`'s
// single `custom_elements: &[C]` slice can carry both element kinds without
// either `codrive/cursor.rs` or `codrive/shadow.rs` needing to know about
// each other or about `GlesRenderer` specifically — this module is the one
// place in the crate that combines them, mirroring why it (not `codrive/`)
// is the one place that already knows about `GlesRenderer` concretely.
// CUR-1 (2026-08-22) added the last two variants. The human pointer is no
// longer a solid rectangle: it is either a themed XCursor image uploaded from
// main memory (`Memory`, `crate::cursor::theme` / `crate::cursor::fallback`)
// or a client-provided cursor surface drawn as a real surface tree
// (`Surface`, `wl_pointer.set_cursor`). Both are per-frame element types
// exactly like the two above, so they belong in the same enum rather than in
// a second `custom_elements` mechanism — `render_output` takes ONE
// `&[C]` slice, and every element in a frame has to be one `C`.
render_elements! {
    pub CodriveElement<=GlesRenderer>;
    Solid=SolidColorRenderElement,
    Pip=TextureRenderElement<GlesTexture>,
    Memory=MemoryRenderBufferRenderElement<GlesRenderer>,
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
}
