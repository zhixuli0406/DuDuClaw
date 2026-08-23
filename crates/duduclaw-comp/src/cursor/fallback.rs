//! CUR-1: the built-in, asset-free pointer used when no XCursor theme can be
//! loaded.
//!
//! # Why this exists at all
//!
//! `duduclaw-comp` runs in two very different places. On the appliance image
//! there is a full Adwaita cursor theme (pulled in by GTK/chromium), so
//! [`crate::cursor::theme`] finds real artwork. In a bare container — every
//! `BUILD.md` verification round, and any minimal nested environment — there
//! is often no `/usr/share/icons` at all.
//!
//! The pre-CUR-1 behaviour in that situation was a 10×10 **white** square
//! (`codrive/cursor.rs`'s CD-0 placeholder), which the user could not see:
//! *"滑鼠是一個方塊，而非主流鼠標，而且還是白色的誰看得到"*. A degraded path
//! must not be an invisible one, so the fallback here is a real arrow with a
//! **dark outline around a light fill** — legible on a light background
//! (OOBE) because of the outline, and on a dark background because of the
//! fill. No external file, no new dependency: the shape is a compile-time
//! ASCII mask rasterised into RGBA at runtime.
//!
//! # Why a bitmap rather than `SolidColorRenderElement` geometry
//!
//! An arrow assembled from solid rectangles would need one render element per
//! step of every diagonal edge (dozens per frame) and would still look like a
//! staircase. One small RGBA buffer goes through the exact same
//! `MemoryRenderBufferRenderElement` path the themed cursors use, so there is
//! one rendering code path for "human cursor", not two.

use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    utils::Transform,
};

/// The classic left-pointing arrow, 15×24 cells.
///
/// `X` = outline, `#` = fill, anything else = transparent. Hotspot is the
/// top-left cell (0, 0), i.e. the arrow's tip — the same convention every
/// XCursor `left_ptr` uses.
const ARROW: [&str; 24] = [
    "X..............",
    "XX.............",
    "X#X............",
    "X##X...........",
    "X###X..........",
    "X####X.........",
    "X#####X........",
    "X######X.......",
    "X#######X......",
    "X########X.....",
    "X#########X....",
    "X##########X...",
    "X###########X..",
    "X############X.",
    "X#############X",
    "X######XXXXXXXX",
    "X###X##X.......",
    "X##X.X##X......",
    "X#X..X##X......",
    "XX....X##X.....",
    "X.....X##X.....",
    ".......X##X....",
    ".......X##X....",
    "........XX.....",
];

/// Cell width of [`ARROW`]. Asserted against the actual rows by a unit test
/// rather than trusted.
const ARROW_W: u32 = 15;
/// Cell height of [`ARROW`].
const ARROW_H: u32 = 24;

/// Outline colour — `stone-900` (`#1c1917`) from the DuDuClaw palette
/// (`CLAUDE.md`, "Surface dark: deep stone"). Near-black, warm rather than
/// blue-black.
const OUTLINE: [u8; 4] = [0x1c, 0x19, 0x17, 0xff];
/// Fill colour — `stone-50` (`#fafaf9`), the palette's light surface.
const FILL: [u8; 4] = [0xfa, 0xfa, 0xf9, 0xff];
/// Fully transparent. Premultiplied alpha (the Wayland/`MemoryRenderBuffer`
/// convention) makes this all-zero, and makes the two opaque colours above
/// premultiplication no-ops — so no premultiply pass is needed anywhere in
/// this file.
const CLEAR: [u8; 4] = [0, 0, 0, 0];

/// One rasterised fallback arrow: RGBA8 pixels plus their dimensions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterizedArrow {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, R,G,B,A per pixel, premultiplied alpha.
    pub rgba: Vec<u8>,
}

/// Integer upscale factor for a requested cursor `size` (logical px).
///
/// Deliberately integer-only nearest-neighbour: the art is a hard-edged
/// pixel mask, and a fractional resample would blur exactly the dark outline
/// that makes it visible. `size / 24` means a 24 px request draws the mask at
/// 1:1 (24 px tall), 48 px doubles it, 96 px quadruples it; anything below 24
/// still gets 1 (never zero — an invisible cursor is the bug being fixed).
pub fn scale_for_size(size: u32) -> u32 {
    (size / ARROW_H).max(1)
}

/// CUR-3: the height, in pixels, the fallback arrow will ACTUALLY be drawn at
/// for a requested `size` — i.e. `24 × scale_for_size(size)`.
///
/// Exists so [`crate::cursor::theme::CursorThemeStore::effective_size`] can
/// report an honest number on a machine with no XCursor theme at all. Because
/// the scale is integer-only (see [`scale_for_size`]), the fallback quantises
/// hard: a 32 px request draws 24 px of arrow and a 64 px request draws 48. A
/// settings page that showed "32" while 24 px of pixels were on screen would
/// be lying, so the honest number is computed rather than assumed equal to the
/// request.
pub fn rasterized_height(size: u32) -> u32 {
    ARROW_H * scale_for_size(size)
}

/// Rasterises [`ARROW`] at the integer scale appropriate for `size`.
pub fn rasterize(size: u32) -> RasterizedArrow {
    let scale = scale_for_size(size);
    let width = ARROW_W * scale;
    let height = ARROW_H * scale;
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);

    for y in 0..height {
        let row = ARROW[(y / scale) as usize].as_bytes();
        for x in 0..width {
            let cell = row.get((x / scale) as usize).copied().unwrap_or(b'.');
            let px = match cell {
                b'X' => OUTLINE,
                b'#' => FILL,
                _ => CLEAR,
            };
            rgba.extend_from_slice(&px);
        }
    }

    RasterizedArrow {
        width,
        height,
        rgba,
    }
}

/// Builds the fallback arrow as a render buffer ready for
/// `MemoryRenderBufferRenderElement::from_buffer`.
///
/// `Fourcc::Abgr8888` — not `Argb8888` — because the pixels above are laid
/// out R,G,B,A in *byte* order, which on a little-endian machine is the
/// ABGR8888 word layout. (The themed path in [`crate::cursor::theme`]
/// deliberately uses `Argb8888` instead: XCursor files store B,G,R,A on disk.
/// Two formats, each correct for its own data — see that module's comment for
/// the sourced reasoning.) `GlesRenderer` maps `Abgr8888` to plain `GL_RGBA`
/// (`gles/format.rs:20`), so this path needs no format extension at all.
pub fn build_buffer(size: u32) -> MemoryRenderBuffer {
    let arrow = rasterize(size);
    MemoryRenderBuffer::from_slice(
        &arrow.rgba,
        Fourcc::Abgr8888,
        (arrow.width as i32, arrow.height as i32),
        1,
        Transform::Normal,
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(a: &RasterizedArrow, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * a.width + x) * 4) as usize;
        [a.rgba[i], a.rgba[i + 1], a.rgba[i + 2], a.rgba[i + 3]]
    }

    #[test]
    fn art_rows_all_have_the_declared_width() {
        // The rasteriser indexes rows by byte offset, so a ragged row would
        // silently produce transparent columns instead of art.
        for (y, row) in ARROW.iter().enumerate() {
            assert_eq!(
                row.len() as u32,
                ARROW_W,
                "row {y} ({row:?}) is not {ARROW_W} cells wide"
            );
        }
        assert_eq!(ARROW.len() as u32, ARROW_H);
    }

    #[test]
    fn art_uses_only_the_three_known_cell_kinds() {
        for row in ARROW.iter() {
            for c in row.bytes() {
                assert!(
                    matches!(c, b'X' | b'#' | b'.'),
                    "unexpected cell {:?} in {row:?}",
                    c as char
                );
            }
        }
    }

    #[test]
    fn scale_is_never_zero_and_grows_by_whole_steps() {
        assert_eq!(scale_for_size(0), 1, "a zero-size request must not vanish");
        assert_eq!(scale_for_size(8), 1);
        assert_eq!(scale_for_size(24), 1);
        assert_eq!(scale_for_size(47), 1);
        assert_eq!(scale_for_size(48), 2);
        assert_eq!(scale_for_size(96), 4);
    }

    #[test]
    fn rasterized_height_matches_what_rasterize_actually_produces() {
        // CUR-3: this number is reported to a settings UI as `effective_size`,
        // so it must be the real pixel height, derived the same way — not an
        // independently-maintained table that could drift from the rasteriser.
        for size in [0, 8, 24, 32, 47, 48, 64, 96, 512] {
            assert_eq!(
                rasterized_height(size),
                rasterize(size).height,
                "requested {size}"
            );
        }
        // The quantisation a UI has to be honest about: two of the five
        // offered steps draw at a smaller size than they ask for.
        assert_eq!(rasterized_height(24), 24);
        assert_eq!(rasterized_height(32), 24, "32 quantises down to one whole arrow");
        assert_eq!(rasterized_height(48), 48);
        assert_eq!(rasterized_height(64), 48, "64 quantises down to two whole arrows");
        assert_eq!(rasterized_height(96), 96);
    }

    #[test]
    fn rasterize_produces_a_full_rgba_buffer_of_the_right_size() {
        let a = rasterize(24);
        assert_eq!((a.width, a.height), (ARROW_W, ARROW_H));
        assert_eq!(a.rgba.len(), (a.width * a.height * 4) as usize);

        let b = rasterize(48);
        assert_eq!((b.width, b.height), (ARROW_W * 2, ARROW_H * 2));
        assert_eq!(b.rgba.len(), (b.width * b.height * 4) as usize);
    }

    #[test]
    fn tip_is_opaque_outline_and_the_far_corner_is_transparent() {
        let a = rasterize(24);
        // (0,0) is the hotspot — it must be a real, opaque pixel or the
        // pointer would visually "start" somewhere other than where it acts.
        assert_eq!(pixel(&a, 0, 0), OUTLINE);
        // Top-right of the bounding box is outside the arrow.
        assert_eq!(pixel(&a, ARROW_W - 1, 0), CLEAR);
    }

    #[test]
    fn body_is_light_fill_wrapped_in_dark_outline() {
        // This is the whole point of the fallback: it must be visible on a
        // light background (dark outline) AND on a dark one (light fill).
        // Row 4 is "X###X..........": outline, fill, outline.
        let a = rasterize(24);
        assert_eq!(pixel(&a, 0, 4), OUTLINE);
        assert_eq!(pixel(&a, 2, 4), FILL);
        assert_eq!(pixel(&a, 4, 4), OUTLINE);
        assert_eq!(pixel(&a, 5, 4), CLEAR);
        assert_ne!(OUTLINE, FILL, "outline and fill must contrast");
        assert_eq!(OUTLINE[3], 0xff, "outline must be fully opaque");
        assert_eq!(FILL[3], 0xff, "fill must be fully opaque");
    }

    #[test]
    fn scaling_replicates_cells_without_blurring() {
        let a = rasterize(48); // scale 2
        // The single tip cell becomes a 2×2 block of the identical colour —
        // no interpolation, no half-transparent fringe.
        for (x, y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            assert_eq!(pixel(&a, x, y), OUTLINE, "tip block pixel ({x},{y})");
        }
        assert_eq!(pixel(&a, 2, 0), CLEAR);
    }
}
