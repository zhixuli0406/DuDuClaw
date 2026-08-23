//! WM-3: the minimize button's `－`, drawn rather than typeset.
//!
//! Same reasoning as [`super::xmark`], and the same output contract (RGBA8,
//! byte order R,G,B,A, **premultiplied** alpha) so it rides the identical
//! `MemoryRenderBuffer` path as the ✕, the title text and the themed cursors.
//!
//! A horizontal bar is even less font-like than a cross: `U+FF0D` and `U+2212`
//! sit at different vertical positions and different widths in the two
//! embedded faces, so a typeset minimize glyph would jump depending on which
//! face answered — inside a 12 px box that is the difference between "aligned
//! with the ✕" and "visibly wrong".
//!
//! The bar is drawn **centred**, full width, with the same 1 px analytic
//! antialiasing ramp `xmark` uses on its diagonals: coverage falls linearly
//! over one pixel around `thickness / 2` away from the centre line.

use super::text::RasterizedText;

/// Stroke thickness of the `－`, in pixels. Matches `xmark`'s so the two
/// glyphs read as the same weight side by side.
const STROKE: f32 = 1.5;

/// Rasterises an antialiased `－` of `size`×`size` pixels in `color`.
///
/// Square (not a wide, short strip) on purpose: the two buffers are centred in
/// identically-sized button boxes by `decor::paint`, so equal extents mean the
/// centring arithmetic is the same for both and the glyphs cannot drift apart.
pub fn rasterize(size: u32, color: [u8; 3]) -> RasterizedText {
    let n = size.max(1);
    let centre = (n - 1) as f32 / 2.0;
    let half = STROKE / 2.0;

    let mut rgba = Vec::with_capacity((n * n * 4) as usize);
    for y in 0..n {
        let d = (y as f32 - centre).abs();
        let a = (half + 0.5 - d).clamp(0.0, 1.0);
        let px = [
            (color[0] as f32 * a).round() as u8,
            (color[1] as f32 * a).round() as u8,
            (color[2] as f32 * a).round() as u8,
            (a * 255.0).round() as u8,
        ];
        for _ in 0..n {
            rgba.extend_from_slice(&px);
        }
    }

    RasterizedText {
        width: n,
        height: n,
        rgba,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(r: &RasterizedText, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * r.width + x) * 4) as usize;
        [r.rgba[i], r.rgba[i + 1], r.rgba[i + 2], r.rgba[i + 3]]
    }

    #[test]
    fn the_buffer_is_square_and_fully_populated() {
        let r = rasterize(12, [0, 0, 0]);
        assert_eq!((r.width, r.height), (12, 12));
        assert_eq!(r.rgba.len(), (12 * 12 * 4) as usize);
    }

    #[test]
    fn the_bar_is_inked_across_the_middle_and_clear_at_the_top_and_bottom() {
        let r = rasterize(13, [0xff, 0xff, 0xff]);
        for x in 0..13 {
            assert!(pixel(&r, x, 6)[3] > 200, "centre row should be inked at x={x}");
            assert_eq!(pixel(&r, x, 0)[3], 0, "top row should be clear at x={x}");
            assert_eq!(pixel(&r, x, 12)[3], 0, "bottom row should be clear at x={x}");
        }
    }

    #[test]
    fn every_row_is_uniform_so_the_bar_has_no_ragged_ends() {
        let r = rasterize(16, [0x11, 0x22, 0x33]);
        for y in 0..16 {
            let first = pixel(&r, 0, y);
            for x in 1..16 {
                assert_eq!(pixel(&r, x, y), first, "row {y} is not uniform at x={x}");
            }
        }
    }

    #[test]
    fn the_bar_is_symmetric_about_the_horizontal_centre() {
        let n = 13u32;
        let r = rasterize(n, [0, 0, 0]);
        for y in 0..n {
            assert_eq!(pixel(&r, 0, y), pixel(&r, 0, n - 1 - y), "mirror mismatch at y={y}");
        }
    }

    #[test]
    fn pixels_are_premultiplied_like_every_other_buffer_in_this_crate() {
        let r = rasterize(16, [0xff, 0x00, 0x00]);
        for p in r.rgba.chunks_exact(4) {
            if p[3] == 0 {
                assert_eq!(&p[0..3], &[0, 0, 0]);
            }
            assert!(p[0] as u16 <= p[3] as u16 + 1, "red channel exceeds alpha: {p:?}");
        }
    }

    #[test]
    fn a_degenerate_size_still_produces_one_pixel() {
        let r = rasterize(0, [0, 0, 0]);
        assert_eq!((r.width, r.height), (1, 1));
        assert_eq!(r.rgba.len(), 4);
    }
}
