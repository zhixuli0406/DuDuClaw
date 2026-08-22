//! Render elements shared by every backend.
//!
//! Extracted from `winit_backend.rs` in the A4-1 round (udev/DRM backend)
//! purely so the two backends can share one definition instead of the udev
//! backend having to `use crate::winit_backend::CodriveElement` — which
//! would make "the real-hardware backend depends on the nested development
//! backend" true at the module-graph level, which it isn't. The macro
//! invocation itself is unchanged from the CD-2 shadow-workspace round; see
//! the doc comment below (moved here verbatim) for why it exists at all.

use smithay::backend::renderer::{
    element::{
        memory::MemoryRenderBufferRenderElement, render_elements,
        solid::SolidColorRenderElement, surface::WaylandSurfaceRenderElement,
        texture::TextureRenderElement,
    },
    gles::{GlesRenderer, GlesTexture},
};

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
